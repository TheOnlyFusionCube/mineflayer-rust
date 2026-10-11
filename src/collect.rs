//! The mineflayer-collectblock plugin surface: `bot.collect_block().collect(...)`.
//!
//! Upstream's collect is a find → goto → dig loop that resolves once every
//! requested block is broken; ours keeps that shape over the U3/U6/U7
//! primitives: [`FindBlocksOpts`] re-searches each iteration (the world
//! changes under us as we dig), [`Goal::GetToBlock`] walks into dig reach,
//! and [`Bot::dig`] does the breaking. Dug blocks then get a bounded
//! drop-vacuum pass — vanilla pickup is contact-only, and digging from
//! reach leaves the drop on the ground otherwise.
//!
//! Upstream emits no events from `collect`; ours reports
//! [`BotEvent::CollectBlockFound`]/[`BotEvent::Collected`]/
//! [`BotEvent::CollectDone`] since they're the only progress signal a
//! single-await Rust API can give mid-loop.

use std::{collections::HashSet, time::Duration};

use azalea::{
    BlockPos, Vec3,
    entity::{EntityKindComponent, Position},
    registry::builtin::{BlockKind, EntityKind},
};

use crate::{
    Bot, BotEvent, Error, Result,
    pathfinder::Goal,
    world::{Block, BlockMatcher, FindBlocksOpts},
};

/// Upstream collectBlock's search radius (`maxDistance: 64` in its
/// examples and docs).
const DEFAULT_MAX_DISTANCE: f64 = 64.;
/// How many find candidates each iteration scans — enough that a few
/// undiggable/unreachable misses in front of a good block don't end the
/// run early.
const CANDIDATE_WINDOW: usize = 64;
/// Drop-vacuum bounds: at most this many goto attempts per dug block …
const MAX_VACUUM_ATTEMPTS: usize = 4;
/// … each attempt resolving or timing out within this span …
const VACUUM_GOTO_TIMEOUT: Duration = Duration::from_secs(4);
/// … and only drops this close to the bot are chased.
const VACUUM_RADIUS: f64 = 10.;
/// A single `goto` can't hang a collect: azalea leaves a partial path
/// unresolved when a candidate turns out unreachable mid-walk, and our
/// `goto` (like upstream's) only resolves on reach/stop. Bound each
/// attempt and move to the next candidate on timeout.
const GOTO_TIMEOUT: Duration = Duration::from_secs(30);

/// What [`CollectBlock::collect`] digs up — the `target` argument.
///
/// The `From` impls cover the upstream forms (one [`Block`], a list of
/// blocks) plus the find-blocks matchers ([`BlockKind`], kind lists, a
/// `Fn(&Block) -> bool` predicate). [`CollectTarget::new`] with the
/// builder methods gives the full form (predicate + count + distance).
pub struct CollectTarget {
    /// Built once and borrowed by [`WorldRef::find_blocks`] every loop
    /// iteration (a predicate can't be cloned, so it can't be rebuilt per
    /// call — the opts own it instead).
    opts: FindBlocksOpts,
    /// How many matching blocks to dig before resolving.
    count: usize,
}

impl CollectTarget {
    /// The full form: any [`BlockMatcher`] source, then `.count(n)`,
    /// `.max_distance(d)`, `.point(v)` to taste.
    pub fn new(matching: impl Into<BlockMatcher>) -> Self {
        Self {
            opts: FindBlocksOpts {
                matching: matching.into(),
                point: None,
                max_distance: Some(DEFAULT_MAX_DISTANCE),
                count: Some(CANDIDATE_WINDOW),
            },
            count: 1,
        }
    }

    /// Dig this many matching blocks (upstream's `collect` on a list
    /// collects all of them). `0` clamps to 1.
    pub fn count(mut self, count: usize) -> Self {
        self.count = count.max(1);
        // the find window must cover the asked-for count or later
        // iterations starve on a full first page
        self.opts.count = Some(self.count.max(CANDIDATE_WINDOW));
        self
    }

    /// `findBlocks`' `maxDistance` — default 64 like upstream.
    pub fn max_distance(mut self, max_distance: f64) -> Self {
        self.opts.max_distance = Some(max_distance);
        self
    }

    /// Fixed search center; defaults to the bot's position re-read every
    /// iteration (the bot moves as it collects).
    pub fn point(mut self, point: Vec3) -> Self {
        self.opts.point = Some(point);
        self
    }

    /// A position-pinned target (one or many `collect these exact blocks`).
    fn positions(positions: Vec<BlockPos>) -> Self {
        let count = positions.len().max(1);
        let mut target = Self::new(move |b: &Block| positions.contains(&b.pos)).count(count);
        target.opts.count = Some(count.max(CANDIDATE_WINDOW));
        target
    }
}

impl From<Block> for CollectTarget {
    /// `collect(block)` — this exact block position (upstream pins the
    /// block object it was handed, not every block of its kind).
    fn from(block: Block) -> Self {
        Self::positions(vec![block.pos])
    }
}
impl From<BlockPos> for CollectTarget {
    /// `collect(blockPos)` — the block at this position.
    fn from(pos: BlockPos) -> Self {
        Self::positions(vec![pos])
    }
}
impl From<BlockKind> for CollectTarget {
    /// `collect(kind)` — the nearest block of this kind.
    fn from(kind: BlockKind) -> Self {
        Self::new(kind)
    }
}
impl From<Vec<BlockKind>> for CollectTarget {
    /// `collect([k1, k2, …])` — one block total, of any listed kind
    /// (a kind list is an alternation, not a shopping list — use
    /// [`CollectTarget::count`] to collect several).
    fn from(kinds: Vec<BlockKind>) -> Self {
        Self::new(kinds)
    }
}
impl<const N: usize> From<[BlockKind; N]> for CollectTarget {
    fn from(kinds: [BlockKind; N]) -> Self {
        Self::new(kinds)
    }
}
impl From<Vec<Block>> for CollectTarget {
    /// `collect([b1, b2, …])` — collect all of these, nearest-first;
    /// upstream's array form.
    fn from(blocks: Vec<Block>) -> Self {
        Self::positions(blocks.iter().map(|b| b.pos).collect())
    }
}
impl From<&[Block]> for CollectTarget {
    fn from(blocks: &[Block]) -> Self {
        Self::positions(blocks.iter().map(|b| b.pos).collect())
    }
}
impl<F> From<F> for CollectTarget
where
    F: Fn(&Block) -> bool + Send + Sync + 'static,
{
    /// `collect(|block| …)` — nearest match of an arbitrary predicate.
    fn from(pred: F) -> Self {
        Self::new(pred)
    }
}

/// What [`CollectBlock::collect`] resolves with — upstream's callback
/// gets no payload, but the dug list is the honest Rust return.
#[derive(Debug)]
pub struct Collected {
    /// The blocks that were broken, in dig order. Their `state` is the
    /// pre-dig snapshot — what was collected, not the air that's there now.
    pub blocks: Vec<Block>,
}

/// The `bot.collectBlock` namespace — a borrowed view onto the [`Bot`],
/// like [`Bot::pathfinder`].
pub struct CollectBlock<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.collectBlock` — the mineflayer-collectblock namespace.
    pub fn collect_block(&self) -> CollectBlock<'_> {
        CollectBlock { bot: self }
    }
}

impl CollectBlock<'_> {
    /// `bot.collectBlock.collect(target)` — dig `target.count` matching
    /// blocks: find the nearest untried candidate, `goto` into reach,
    /// `dig` it, vacuum its drops, repeat.
    ///
    /// A candidate that fails to path to, disappears mid-walk, or fails
    /// its dig is skipped and never retried within the call. Resolves
    /// `Err` when nothing at all could be collected (upstream rejects on
    /// its first failure); a partial haul resolves `Ok` — check
    /// `collected.blocks.len()` against the count you asked for. There is
    /// no `cancelTask`: drop the future to abort mid-collect.
    pub async fn collect(&self, target: impl Into<CollectTarget>) -> Result<Collected> {
        let mut target = target.into();
        let bot = self.bot;
        if !bot.logged_in() {
            return Err(Error::NotConnected);
        }
        let Some(world) = bot.world() else {
            return Err(Error::NotConnected);
        };
        // `point: None` re-centers on the bot each iteration; a caller-set
        // point is fixed for the whole run
        let fixed_point = target.opts.point;
        let mut tried: HashSet<BlockPos> = HashSet::new();
        let mut collected: Vec<Block> = Vec::new();
        let mut last_err = None;
        while collected.len() < target.count {
            target.opts.point = fixed_point.or_else(|| Some(bot.position()));
            let Some(pos) = world
                .find_blocks(&target.opts)
                .into_iter()
                .find(|p| !tried.contains(p))
            else {
                break;
            };
            let Some(block) = world.block_at(pos) else {
                tried.insert(pos);
                continue;
            };
            // position-pinned matchers still match the air left behind by
            // a dig, so gate on `is_air` too, not just `diggable`
            if block.state.is_air() || !block.diggable() {
                tried.insert(pos);
                continue;
            }
            bot.emit_event(BotEvent::CollectBlockFound(block.clone()));
            let pf = bot.pathfinder();
            match tokio::time::timeout(GOTO_TIMEOUT, pf.goto(Goal::GetToBlock(pos))).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tried.insert(pos);
                    last_err = Some(e);
                    continue;
                }
                Err(_) => {
                    // a stalled path stays installed — stop it before the
                    // next candidate or `goto` sees the old goal
                    bot.pathfinder().stop();
                    tried.insert(pos);
                    last_err = Some(Error::Operation(format!(
                        "path to {pos} timed out after {GOTO_TIMEOUT:?}"
                    )));
                    continue;
                }
            }
            // re-resolve after the walk — the block may have been dug by
            // someone else or changed while we travelled
            if !world.find_blocks(&target.opts).contains(&pos) {
                tried.insert(pos);
                continue;
            }
            let Some(block) = world.block_at(pos) else {
                tried.insert(pos);
                continue;
            };
            if block.state.is_air() || !block.diggable() {
                tried.insert(pos);
                continue;
            }
            match bot.dig(&block).await {
                Ok(()) => {
                    bot.emit_event(BotEvent::Collected(block.clone()));
                    collected.push(block);
                    tried.insert(pos); // it's air now — don't re-find it
                    self.vacuum_drops().await;
                }
                Err(e) => {
                    tried.insert(pos);
                    last_err = Some(e);
                }
            }
        }
        if collected.is_empty() {
            return Err(last_err
                .unwrap_or_else(|| Error::Operation("no matching blocks found".to_string())));
        }
        bot.emit_event(BotEvent::CollectDone {
            collected: collected.len(),
        });
        Ok(Collected { blocks: collected })
    }

    /// Walk over item-drop entities near the bot so vanilla pickup grabs
    /// them. Best-effort and bounded: a drop that never spawns, lands out
    /// of reach, or sits in lava costs at most the attempts/timeout above,
    /// then collect moves on — the drop is reported dug either way.
    async fn vacuum_drops(&self) {
        // drops spawn a tick or two after the break lands server-side
        let _ = self.bot.wait_for_ticks(2).await;
        for _ in 0..MAX_VACUUM_ATTEMPTS {
            let my = self.bot.position();
            let Some(drop) = self.bot.nearest_entity(|e| {
                e.get_component::<EntityKindComponent>()
                    .is_some_and(|k| k.0 == EntityKind::Item)
                    && e.get_component::<Position>()
                        .is_some_and(|p| p.distance_to(my) <= VACUUM_RADIUS)
            }) else {
                return;
            };
            let Some(pos) = drop.get_component::<Position>().map(|p| **p) else {
                return;
            };
            let pf = self.bot.pathfinder();
            let goto = pf.goto(Goal::Near { pos, radius: 1. });
            if tokio::time::timeout(VACUUM_GOTO_TIMEOUT, goto)
                .await
                .is_err()
            {
                // stop the orphaned chase so the next dig's goto starts clean
                self.bot.pathfinder().stop();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use azalea::{block::BlockState, registry::builtin::BlockKind};

    use super::*;
    use crate::{test_util::despawned_client, world::BlockMatcher};

    #[test]
    fn kind_target_is_a_state_matcher_with_upstream_defaults() {
        let target = CollectTarget::from(BlockKind::Stone);
        let BlockMatcher::States(states) = &target.opts.matching else {
            panic!("a block kind must match on palette states, not a predicate")
        };
        assert!(states.contains(&BlockState::from(BlockKind::Stone)));
        assert!(!states.contains(&BlockState::from(BlockKind::Dirt)));
        assert_eq!(target.count, 1);
        assert_eq!(target.opts.max_distance, Some(DEFAULT_MAX_DISTANCE));
        // the find window, not the requested count
        assert_eq!(target.opts.count, Some(CANDIDATE_WINDOW));
        // None = re-center on the bot every iteration
        assert!(target.opts.point.is_none());
    }

    #[test]
    fn builder_tunes_count_distance_and_point() {
        let target = CollectTarget::new(BlockKind::Sand)
            .count(5)
            .max_distance(30.)
            .point(Vec3::new(1., 2., 3.));
        assert_eq!(target.count, 5);
        assert_eq!(target.opts.max_distance, Some(30.));
        assert_eq!(target.opts.point, Some(Vec3::new(1., 2., 3.)));
        // the find window grows to cover the asked-for count
        assert_eq!(target.opts.count, Some(5usize.max(CANDIDATE_WINDOW)));
        // `0` clamps to 1 like upstream's "collect something" contract
        assert_eq!(CollectTarget::new(BlockKind::Sand).count(0).count, 1);
        // a huge count widens the window past CANDIDATE_WINDOW
        assert_eq!(
            CollectTarget::new(BlockKind::Sand).count(200).opts.count,
            Some(200)
        );
    }

    #[test]
    fn block_target_pins_its_position_not_its_kind() {
        let client = despawned_client();
        let block = Block::new(
            &client,
            BlockPos::new(3, 64, -5),
            BlockState::from(BlockKind::Stone),
        );
        let target = CollectTarget::from(block.clone());
        assert_eq!(target.count, 1);
        let BlockMatcher::Predicate(pred) = &target.opts.matching else {
            panic!("a pinned position must match on position, not state")
        };
        assert!(pred(&block));
        // same kind, wrong position — upstream collects the handed block
        let same_kind_elsewhere = Block::new(
            &client,
            BlockPos::new(4, 64, -5),
            BlockState::from(BlockKind::Stone),
        );
        assert!(!pred(&same_kind_elsewhere));
    }

    #[test]
    fn block_list_collects_every_listed_position() {
        let client = despawned_client();
        let blocks = vec![
            Block::new(
                &client,
                BlockPos::new(0, 60, 0),
                BlockState::from(BlockKind::OakLog),
            ),
            Block::new(
                &client,
                BlockPos::new(8, 62, -4),
                BlockState::from(BlockKind::OakLog),
            ),
        ];
        let target = CollectTarget::from(blocks.clone());
        // upstream's array form collects all of them
        assert_eq!(target.count, 2);
        let BlockMatcher::Predicate(pred) = &target.opts.matching else {
            panic!("positions match by predicate")
        };
        assert!(blocks.iter().all(pred));
        assert_eq!(target.opts.count, Some(2usize.max(CANDIDATE_WINDOW)));
    }

    #[test]
    fn arbitrary_predicate_target_passes_through() {
        let target = CollectTarget::new(|b: &Block| b.state.is_air());
        assert!(matches!(target.opts.matching, BlockMatcher::Predicate(_)));
        assert_eq!(target.count, 1);
    }

    #[tokio::test]
    async fn collect_before_spawn_is_not_connected() {
        let bot = Bot::new(despawned_client());
        let err = bot
            .collect_block()
            .collect(BlockKind::Stone)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotConnected));
    }
}
