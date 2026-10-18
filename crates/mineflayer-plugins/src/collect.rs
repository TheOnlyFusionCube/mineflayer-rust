//! The mineflayer-collectblock plugin surface:
//! `bot.collect_block().collect(...)`.
//!
//! Upstream's collect is a find → goto → dig loop that resolves once every
//! requested block is broken; ours keeps that shape over the find, goto,
//! and dig primitives: [`FindBlocksOpts`] re-searches each iteration (the world
//! changes under us as we dig), [`Goal::LookAtBlock`] walks into sight
//! and dig reach like upstream's `mineBlock` goal, and [`Bot::dig`] does
//! the breaking. Before digging, the loop equips
//! the best harvest tool the way upstream's `mineBlock` does
//! (`requireHarvest` + `getFromChest`). Dug blocks then get a bounded
//! drop-vacuum pass — vanilla pickup is contact-only, and digging from
//! reach leaves the drop on the ground otherwise.
//!
//! Upstream emits `collectBlock_finished` from `collect`'s `finally` —
//! [`CollectEvent::Finished`] mirrors it on every exit.
//! [`CollectEvent::Found`]/[`CollectEvent::Collected`]/
//! [`CollectEvent::Done`] stay additive progress reports the
//! single-await API can't otherwise give.

use std::{collections::HashSet, sync::Arc, time::Duration};

// `mineflayer::engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// real crate name in scope.
use mineflayer::engine::ecs as bevy_ecs;
use mineflayer::{
    Block, BlockMatcher, Bot, Error, FindBlocksOpts, Goal, Item, Movements, Result, WorldRef,
    engine::{
        BlockPos, Vec3,
        ecs::prelude::Component,
        entity::{EntityKindComponent, Position},
        registry::builtin::{BlockKind, EntityKind},
    },
    plugin::{Op, OpSlot},
};
use tokio::sync::broadcast;

use crate::tool::{EquipOpts, ToolExt, can_harvest};

/// The collectblock plugin's events; subscribe with
/// [`Bot::plugin_events::<CollectEvent>`](Bot::plugin_events).
#[derive(Clone, Debug)]
pub enum CollectEvent {
    /// `collect` targeted a matching block (before walking to it).
    Found(Block),
    /// `collect` broke a target block.
    Collected(Block),
    /// `collect` finished — `collected` is how many blocks were broken
    /// (short of the requested count when the search exhausted early).
    Done {
        /// Blocks broken by the completed `collect` call.
        collected: usize,
    },
    /// mineflayer-collectblock `collectBlock_finished` — a `collect` call
    /// concluded, successfully or not (upstream emits it from a
    /// `finally`). [`CollectEvent::Done`] stays the per-run success
    /// report; a cancelled collect emits this without it.
    Finished,
}

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
/// A single `goto` can't hang a collect: the engine leaves a partial path
/// unresolved when a candidate turns out unreachable mid-walk, and our
/// `goto` (like upstream's) only resolves on reach/stop. Bound each
/// attempt and move to the next candidate on timeout.
const GOTO_TIMEOUT: Duration = Duration::from_secs(30);

/// `ItemFilter` — upstream's `itemFilter(item) -> bool`, used when
/// emptying a full inventory into a registered chest.
pub type ItemFilter = Arc<dyn Fn(&Item) -> bool + Send + Sync>;

/// Upstream's default `itemFilter` — everything except tools, weapons,
/// and armor (the name checks from `CollectBlock.ts`).
fn default_item_filter() -> ItemFilter {
    Arc::new(|item: &Item| {
        let name = item.name();
        ![
            "helmet",
            "chestplate",
            "leggings",
            "boots",
            "shield",
            "sword",
            "pickaxe",
            "axe",
            "shovel",
            "hoe",
        ]
        .iter()
        .any(|t| name.contains(t))
    })
}

/// `CollectOptions` — the options bag for [`CollectBlock::collect_opts`].
#[derive(Default)]
pub struct CollectOpts {
    /// `append` — share the in-flight collect's fate instead of
    /// cancelling it (upstream appends targets to the running task; ours
    /// runs the calls side by side — `cancel_task` still ends them
    /// together, like upstream).
    pub append: bool,
    /// `ignoreNoPath` — a target whose `goto` fails is skipped for this
    /// run instead of rejecting the whole collect. Upstream declares
    /// the option but never implements it (`// TODO:
    /// options.ignoreNoPath`), so its real behavior is the default
    /// `false` = first path failure rejects.
    pub ignore_no_path: bool,
    /// `chestLocations` — override the plugin's chest list for this
    /// call (upstream `options.chestLocations ?? this.chestLocations`).
    pub chest_locations: Option<Vec<BlockPos>>,
    /// `itemFilter` — override the plugin's filter for this call
    /// (upstream `options.itemFilter ?? this.itemFilter`).
    pub item_filter: Option<ItemFilter>,
}

/// Facade state the engine has no slot for — the plugin's mutable fields
/// (`chestLocations`, `itemFilter`, `movements`) plus the cancellation
/// bookkeeping for `cancelTask`/`append`.
/// Stored as an ECS component created on first use — rule 2 of the
/// storage rule on `bot::BotShared`.
#[derive(Component)]
struct CollectState {
    /// The running collect: a non-`append` collect and `cancel_task`
    /// cancel it, and an `append` collect joins it.
    task: OpSlot,
    /// How many collect calls are in flight (`append` can overlap them).
    active: usize,
    /// Plugin `chestLocations`.
    chest_locations: Vec<BlockPos>,
    /// Plugin `itemFilter` — upstream's default.
    item_filter: ItemFilter,
    /// Plugin `movements` — applied per `collect` like upstream's
    /// `pathfinder.setMovements` (`None` leaves the config alone).
    movements: Option<Movements>,
}

impl Default for CollectState {
    fn default() -> Self {
        Self {
            task: OpSlot::default(),
            active: 0,
            chest_locations: vec![],
            item_filter: default_item_filter(),
            movements: Some(Movements::default()),
        }
    }
}

/// What one candidate block came to in [`CollectBlock::attempt`].
enum Attempt {
    Collected(Block),
    /// Skipped; the error is reported at the end if nothing was collected.
    Skipped(Option<Error>),
    Cancelled,
}

/// `true` when `cancel_task` or a newer non-`append` collect ended this
/// run. A disconnect is not a cancel: the run's next call reports it.
fn stale(bot: &Bot, op: &Op) -> bool {
    op.is_cancelled() && !bot.is_disconnected()
}

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

/// Adds `collect_block` to [`Bot`].
pub trait CollectExt {
    /// `bot.collectBlock` — the mineflayer-collectblock namespace.
    fn collect_block(&self) -> CollectBlock<'_>;
}

impl CollectExt for Bot {
    fn collect_block(&self) -> CollectBlock<'_> {
        CollectBlock { bot: self }
    }
}

impl CollectBlock<'_> {
    /// `bot.collectBlock.chestLocations` — chests a full inventory may
    /// be emptied into mid-collect (upstream's public array).
    pub fn chest_locations(&self) -> Vec<BlockPos> {
        self.bot
            .state::<CollectState, _>(|s| s.chest_locations.clone())
            .unwrap_or_default()
    }

    /// Replace `chestLocations`.
    pub fn set_chest_locations(&self, locations: Vec<BlockPos>) {
        self.bot
            .state_mut::<CollectState, _>(|s| s.chest_locations = locations);
    }

    /// `chestLocations.push(pos)`.
    pub fn add_chest_location(&self, pos: BlockPos) {
        self.bot
            .state_mut::<CollectState, _>(|s| s.chest_locations.push(pos));
    }

    /// `bot.collectBlock.itemFilter` — the chest-deposit filter
    /// (upstream's default keeps tools, weapons, and armor).
    pub fn item_filter(&self) -> ItemFilter {
        self.bot
            .state::<CollectState, _>(|s| s.item_filter.clone())
            .unwrap_or_else(default_item_filter)
    }

    /// Replace `itemFilter`.
    pub fn set_item_filter(&self, filter: ItemFilter) {
        self.bot
            .state_mut::<CollectState, _>(|s| s.item_filter = filter);
    }

    /// `bot.collectBlock.movements` — applied to the pathfinder on
    /// `collect` (default `Some(Movements::default())`; `None` leaves
    /// the pathfinder's config alone).
    pub fn movements(&self) -> Option<Movements> {
        self.bot
            .state::<CollectState, _>(|s| s.movements.clone())
            .unwrap_or_else(|| Some(Movements::default()))
    }

    /// Set `movements`.
    pub fn set_movements(&self, movements: Option<Movements>) {
        self.bot
            .state_mut::<CollectState, _>(|s| s.movements = movements);
    }

    /// `bot.collectBlock.collect(target)` — the default-options call.
    pub async fn collect(&self, target: impl Into<CollectTarget>) -> Result<Collected> {
        self.collect_opts(target, CollectOpts::default()).await
    }

    /// `bot.collectBlock.collect(target, options)` — dig `target.count`
    /// matching blocks: empty the inventory into a chest when full, find
    /// the nearest untried candidate, `goto` into reach, equip the best
    /// harvest tool, `dig` it, vacuum its drops, repeat.
    ///
    /// A candidate that fails to path to, disappears mid-walk, or fails
    /// its dig is skipped and never retried within the call — except a
    /// `goto` failure with `ignore_no_path` off, which rejects the whole
    /// collect like upstream's uncaught path error. Resolves `Err` when
    /// nothing at all could be collected; a partial haul resolves `Ok`
    /// — check `collected.blocks.len()` against the count you asked
    /// for. [`CollectBlock::cancel_task`] aborts in-flight runs; they
    /// resolve `Ok` with whatever was dug, like upstream's
    /// `PathStopped`-swallowing catch.
    pub async fn collect_opts(
        &self,
        target: impl Into<CollectTarget>,
        opts: CollectOpts,
    ) -> Result<Collected> {
        let mut target = target.into();
        let bot = self.bot;
        if !bot.logged_in() {
            return Err(Error::NotConnected);
        }
        let Some(world) = bot.world() else {
            return Err(Error::NotConnected);
        };
        // non-append collects retire the in-flight run the way
        // upstream's `await this.cancelTask()` does; append shares its
        // operation (and its fate under a later cancel)
        let Some((op, chest_locations, item_filter, movements)) =
            bot.state_mut::<CollectState, _>(|s| {
                s.active += 1;
                (
                    if opts.append {
                        bot.join_op(&mut s.task)
                    } else {
                        bot.start_op(&mut s.task)
                    },
                    opts.chest_locations
                        .unwrap_or_else(|| s.chest_locations.clone()),
                    opts.item_filter.unwrap_or_else(|| s.item_filter.clone()),
                    s.movements.clone(),
                )
            })
        else {
            return Err(Error::NotConnected);
        };
        // upstream mutates and pushes the plugin movements per collect
        if let Some(mut movements) = movements {
            movements.dont_mine_under_falling_block = false;
            bot.pathfinder().set_movements(movements);
        }
        let result = self
            .run(
                &mut target,
                &world,
                &op,
                &chest_locations,
                &item_filter,
                opts.ignore_no_path,
            )
            .await;
        bot.state_mut::<CollectState, _>(|s| s.active = s.active.saturating_sub(1));
        // upstream emits collectBlock_finished from `finally` — on
        // success, error, and cancel alike
        bot.emit_plugin_event(CollectEvent::Finished);
        result
    }

    /// The find → goto → equip → dig → vacuum loop. Once `op` is
    /// cancelled the run wraps up like
    /// upstream's `PathStopped` catch — returning the partial haul, no
    /// error.
    async fn run(
        &self,
        target: &mut CollectTarget,
        world: &WorldRef,
        op: &Op,
        chest_locations: &[BlockPos],
        item_filter: &ItemFilter,
        ignore_no_path: bool,
    ) -> Result<Collected> {
        let bot = self.bot;
        // `point: None` re-centers on the bot each iteration; a caller-set
        // point is fixed for the whole run
        let fixed_point = target.opts.point;
        let mut tried: HashSet<BlockPos> = HashSet::new();
        let mut collected: Vec<Block> = Vec::new();
        let mut last_err = None;
        let mut cancelled = false;
        while collected.len() < target.count {
            if stale(bot, op) {
                cancelled = true;
                break;
            }
            if let Err(e) = self
                .empty_inventory_if_full(chest_locations, item_filter)
                .await
            {
                if stale(bot, op) {
                    cancelled = true;
                    break;
                }
                return Err(e);
            }
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
            bot.emit_plugin_event(CollectEvent::Found(block.clone()));
            let attempt = self
                .attempt(world, &target.opts, pos, op, ignore_no_path)
                .await?;
            tried.insert(pos);
            match attempt {
                Attempt::Collected(block) => {
                    bot.emit_plugin_event(CollectEvent::Collected(block.clone()));
                    collected.push(block);
                    self.vacuum_drops(op).await;
                }
                Attempt::Skipped(err) => last_err = err.or(last_err),
                Attempt::Cancelled => {
                    cancelled = true;
                    break;
                }
            }
        }
        if cancelled {
            return Ok(Collected { blocks: collected });
        }
        if collected.is_empty() {
            return Err(
                last_err.unwrap_or_else(|| Error::NotFound("no matching blocks found".to_string()))
            );
        }
        bot.emit_plugin_event(CollectEvent::Done {
            collected: collected.len(),
        });
        Ok(Collected { blocks: collected })
    }

    /// Walk to the candidate at `pos`, check it again, equip the best
    /// harvest tool, and dig it (upstream `mineBlock`). An error is
    /// returned only when it ends the whole collect.
    async fn attempt(
        &self,
        world: &WorldRef,
        opts: &FindBlocksOpts,
        pos: BlockPos,
        op: &Op,
        ignore_no_path: bool,
    ) -> Result<Attempt> {
        let bot = self.bot;
        // upstream's mineBlock paths with `GoalLookAtBlock` — get within
        // sight+reach of a face, not strictly adjacent (`cb.ts:24`)
        let walked =
            match tokio::time::timeout(GOTO_TIMEOUT, bot.pathfinder().goto(Goal::LookAtBlock(pos)))
                .await
            {
                Ok(result) => result,
                Err(_) => {
                    // a stalled path stays installed — stop it before the
                    // next candidate or `goto` sees the old goal
                    bot.pathfinder().stop();
                    Err(Error::Failed(format!(
                        "path to {pos} timed out after {GOTO_TIMEOUT:?}"
                    )))
                }
            };
        if stale(bot, op) {
            return Ok(Attempt::Cancelled);
        }
        if let Err(e) = walked {
            return if ignore_no_path {
                Ok(Attempt::Skipped(Some(e)))
            } else {
                Err(e)
            };
        }
        // re-resolve after the walk — the block may have been dug by
        // someone else or changed while we travelled
        if !world.find_blocks(opts).contains(&pos) {
            return Ok(Attempt::Skipped(None));
        }
        let Some(block) = world.block_at(pos) else {
            return Ok(Attempt::Skipped(None));
        };
        if block.state.is_air() || !block.diggable() {
            return Ok(Attempt::Skipped(None));
        }
        // upstream mineBlock equips the best harvest tool first and skips
        // blocks the held item can't harvest
        let equipped = bot
            .tool()
            .equip_for_block_opts(
                &block,
                &EquipOpts {
                    require_harvest: true,
                    get_from_chest: true,
                    max_tools: Some(2),
                },
            )
            .await;
        if let Err(e) = equipped {
            return if stale(bot, op) {
                Ok(Attempt::Cancelled)
            } else {
                Err(e)
            };
        }
        if let Some(held) = bot.held_item()
            && !can_harvest(block.state, &held.stack)
        {
            return Ok(Attempt::Skipped(None));
        }
        Ok(match bot.dig(&block).await {
            Ok(()) => Attempt::Collected(block),
            Err(e) => Attempt::Skipped(Some(e)),
        })
    }

    /// `bot.collectBlock.cancelTask()` — retire every in-flight collect
    /// and wait out the `collectBlock_finished` they emit, like
    /// upstream's `once(bot, 'collectBlock_finished')` after
    /// `pathfinder.stop()`. A no-op when nothing is running.
    pub async fn cancel_task(&self) {
        let bot = self.bot;
        // subscribe before the state check so a run finishing between
        // the two can't lose its `finished` event
        let mut rx = bot.plugin_events::<CollectEvent>();
        let active = bot
            .state_mut::<CollectState, _>(|s| {
                if s.active == 0 {
                    return false;
                }
                s.task.cancel();
                true
            })
            .unwrap_or(false);
        if !active {
            return;
        }
        // upstream `bot.pathfinder.stop()` — wakes a parked goto so the
        // loop sees its cancel promptly
        bot.pathfinder().stop();
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(CollectEvent::Finished) => return,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = bot.disconnected() => return,
            }
        }
    }

    /// `bot.collectBlock.findFromVein(block)` — flood-fill the touching
    /// same-kind blocks (upstream defaults `maxBlocks=100`,
    /// `maxDistance=16`, `floodRadius=1`), for handing back to
    /// `collect` as a target list.
    pub fn find_from_vein(&self, block: &Block) -> Vec<Block> {
        self.find_from_vein_opts(block, 100, 16., 1)
    }

    /// `findFromVein(block, maxBlocks, maxDistance, floodRadius)` —
    /// the full upstream signature.
    pub fn find_from_vein_opts(
        &self,
        block: &Block,
        max_blocks: usize,
        max_distance: f64,
        flood_radius: i32,
    ) -> Vec<Block> {
        let Some(world) = self.bot.world() else {
            return vec![];
        };
        let positions = {
            let guard = world.client().world();
            let world = guard.read();
            vein_positions(
                &world,
                block.pos,
                block.kind(),
                max_blocks,
                max_distance,
                flood_radius,
            )
        };
        positions
            .into_iter()
            .filter_map(|p| world.block_at(p))
            .collect()
    }

    /// `emptyInventoryIfFull(bot, chestLocations, itemFilter)` — deposit
    /// the filter-passing inventory stacks into the nearest non-full
    /// registered chest. No-ops while a slot is free; errors `NoChests`
    /// like upstream when the inventory is full and no chest can take
    /// the overflow. `collect` calls this internally with the configured
    /// (or per-call) chest list and filter.
    pub async fn empty_inventory_if_full(
        &self,
        chest_locations: &[BlockPos],
        item_filter: &ItemFilter,
    ) -> Result<()> {
        let bot = self.bot;
        if bot.inventory().empty_slot_count() > 0 {
            return Ok(());
        }
        if chest_locations.is_empty() {
            return Err(Error::NotFound(
                "NoChests: There are no defined chest locations!".into(),
            ));
        }
        let mut locations = chest_locations.to_vec();
        loop {
            let my = bot.position();
            let Some((nearest, _)) = locations
                .iter()
                .enumerate()
                .map(|(i, p)| (i, my.distance_to(p.center())))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            else {
                return Err(Error::NotFound("NoChests: All chests are full.".into()));
            };
            let chest_pos = locations.remove(nearest);
            let pf = bot.pathfinder();
            tokio::time::timeout(GOTO_TIMEOUT, pf.goto(Goal::GetToBlock(chest_pos)))
                .await
                .map_err(|_| {
                    Error::Failed(format!(
                        "path to {chest_pos} timed out after {GOTO_TIMEOUT:?}"
                    ))
                })??;
            let Some(block) = bot.block_at(chest_pos) else {
                return Err(Error::NotFound(
                    "UnloadedChunk: Chest is in an unloaded chunk!".into(),
                ));
            };
            let chest = bot.open_chest(&block).await?;
            // upstream `hasRemaining` — the filter kept items that
            // didn't fit
            let mut has_remaining = false;
            for item in chest.inventory_items() {
                if !item_filter(&item) {
                    continue;
                }
                if chest
                    .first_empty_slot_range(chest.container_range())
                    .is_none()
                {
                    has_remaining = true;
                    break;
                }
                chest
                    .deposit(item.kind(), item.metadata(), Some(item.count()), None)
                    .await?;
            }
            chest.close();
            if !has_remaining {
                return Ok(());
            }
        }
    }

    /// Walk over item-drop entities near the bot so vanilla pickup grabs
    /// them. Best-effort and bounded: a drop that never spawns, lands out
    /// of reach, or sits in lava costs at most the attempts/timeout above,
    /// then collect moves on — the drop is reported dug either way.
    async fn vacuum_drops(&self, op: &Op) {
        // drops spawn a tick or two after the break lands server-side
        let _ = self.bot.wait_for_ticks(2).await;
        for _ in 0..MAX_VACUUM_ATTEMPTS {
            if stale(self.bot, op) {
                return;
            }
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

/// Upstream `BlockVeins.js`: breadth-first flood fill over blocks of the
/// same kind. Neighbors within `flood_radius` of each visited block are
/// considered "touching"; candidates farther than `max_distance`
/// (manhattan) from `start` are skipped.
fn vein_positions(
    world: &mineflayer::engine::world::World,
    start: BlockPos,
    kind: BlockKind,
    max_blocks: usize,
    max_distance: f64,
    flood_radius: i32,
) -> Vec<BlockPos> {
    let mut targets = Vec::new();
    let mut open = vec![start];
    // a position enters `open` exactly once — `seen` covers upstream's
    // `targets.includes` + `open.includes` checks
    let mut seen = HashSet::from([start]);
    while targets.len() < max_blocks {
        let Some(next) = open.pop() else {
            break;
        };
        targets.push(next);
        for dx in -flood_radius..=flood_radius {
            for dy in -flood_radius..=flood_radius {
                for dz in -flood_radius..=flood_radius {
                    let pos = next + BlockPos::new(dx, dy, dz);
                    let manhattan = ((pos.x - start.x).abs()
                        + (pos.y - start.y).abs()
                        + (pos.z - start.z).abs()) as f64;
                    if manhattan > max_distance || !seen.insert(pos) {
                        continue;
                    }
                    match world.get_block_state(pos) {
                        Some(state) if state.as_block_kind() == kind => open.push(pos),
                        _ => {}
                    }
                }
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests;
