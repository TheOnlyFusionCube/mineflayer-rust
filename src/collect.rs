//! The mineflayer-collectblock plugin surface: `bot.collect_block().collect(...)`.
//!
//! Upstream's collect is a find → goto → dig loop that resolves once every
//! requested block is broken; ours keeps that shape over the U3/U6/U7
//! primitives: [`FindBlocksOpts`] re-searches each iteration (the world
//! changes under us as we dig), [`Goal::LookAtBlock`] walks into sight
//! and dig reach like upstream's `mineBlock` goal, and [`Bot::dig`] does
//! the breaking. Before digging, the loop equips
//! the best harvest tool the way upstream's `mineBlock` does
//! (`requireHarvest` + `getFromChest`). Dug blocks then get a bounded
//! drop-vacuum pass — vanilla pickup is contact-only, and digging from
//! reach leaves the drop on the ground otherwise.
//!
//! Upstream emits `collectBlock_finished` from `collect`'s `finally` —
//! [`BotEvent::CollectBlockFinished`] mirrors it on every exit.
//! [`BotEvent::CollectBlockFound`]/[`BotEvent::Collected`]/
//! [`BotEvent::CollectDone`] stay additive progress reports the
//! single-await API can't otherwise give.

use std::{collections::HashSet, sync::Arc, time::Duration};

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// real crate name in scope.
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    BlockPos, Vec3,
    ecs::prelude::Component,
    entity::{EntityKindComponent, Position},
    registry::builtin::{BlockKind, EntityKind},
};
use tokio::sync::broadcast;

use crate::{
    Bot, BotEvent, Error, Result,
    pathfinder::{Goal, Movements},
    tool::{EquipOpts, can_harvest},
    windows::Item,
    world::{Block, BlockMatcher, FindBlocksOpts, WorldRef},
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
    /// The in-flight collects' generation — `collect` snapshots it,
    /// `cancel_task` and non-`append` collects bump it; a stale
    /// generation is the cancellation signal (the flag-shape
    /// [`crate::pvp`] drivers use).
    generation: u64,
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
            generation: 0,
            active: 0,
            chest_locations: vec![],
            item_filter: default_item_filter(),
            movements: Some(Movements::default()),
        }
    }
}

/// Read from the [`CollectState`] component; `None` pre-spawn.
fn collect_state<R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&CollectState) -> R,
) -> Option<R> {
    let ecs = client.ecs.read();
    let entity = ecs.get_entity(client.entity).ok()?;
    entity.get::<CollectState>().map(f)
}

/// Run `f` against [`CollectState`], inserting a default one first.
fn collect_state_mut<R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&mut CollectState) -> R,
) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    if entity.get::<CollectState>().is_none() {
        entity.insert(CollectState::default());
    }
    entity.get_mut::<CollectState>().map(|mut c| f(&mut c))
}

/// `true` when `generation` went stale — `cancel_task` or a newer
/// non-`append` collect retired this run (or the state itself is gone).
fn stale(client: &mineflayer_engine::Client, generation: u64) -> bool {
    collect_state(client, |s| s.generation != generation).unwrap_or(true)
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

impl Bot {
    /// `bot.collectBlock` — the mineflayer-collectblock namespace.
    pub fn collect_block(&self) -> CollectBlock<'_> {
        CollectBlock { bot: self }
    }
}

impl CollectBlock<'_> {
    /// `bot.collectBlock.chestLocations` — chests a full inventory may
    /// be emptied into mid-collect (upstream's public array).
    pub fn chest_locations(&self) -> Vec<BlockPos> {
        collect_state(&self.bot.client, |s| s.chest_locations.clone()).unwrap_or_default()
    }

    /// Replace `chestLocations`.
    pub fn set_chest_locations(&self, locations: Vec<BlockPos>) {
        collect_state_mut(&self.bot.client, |s| s.chest_locations = locations);
    }

    /// `chestLocations.push(pos)`.
    pub fn add_chest_location(&self, pos: BlockPos) {
        collect_state_mut(&self.bot.client, |s| s.chest_locations.push(pos));
    }

    /// `bot.collectBlock.itemFilter` — the chest-deposit filter
    /// (upstream's default keeps tools, weapons, and armor).
    pub fn item_filter(&self) -> ItemFilter {
        collect_state(&self.bot.client, |s| s.item_filter.clone())
            .unwrap_or_else(default_item_filter)
    }

    /// Replace `itemFilter`.
    pub fn set_item_filter(&self, filter: ItemFilter) {
        collect_state_mut(&self.bot.client, |s| s.item_filter = filter);
    }

    /// `bot.collectBlock.movements` — applied to the pathfinder on
    /// `collect` (default `Some(Movements::default())`; `None` leaves
    /// the pathfinder's config alone).
    pub fn movements(&self) -> Option<Movements> {
        collect_state(&self.bot.client, |s| s.movements.clone())
            .unwrap_or_else(|| Some(Movements::default()))
    }

    /// Set `movements`.
    pub fn set_movements(&self, movements: Option<Movements>) {
        collect_state_mut(&self.bot.client, |s| s.movements = movements);
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
        // generation (and its fate under a later cancel)
        let Some((generation, chest_locations, item_filter, movements)) =
            collect_state_mut(&bot.client, |s| {
                if !opts.append {
                    s.generation += 1;
                }
                s.active += 1;
                (
                    s.generation,
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
            movements.dont_create_flow = false;
            bot.pathfinder().set_movements(movements);
        }
        let result = self
            .run(
                &mut target,
                &world,
                generation,
                &chest_locations,
                &item_filter,
                opts.ignore_no_path,
            )
            .await;
        collect_state_mut(&bot.client, |s| s.active = s.active.saturating_sub(1));
        // upstream emits collectBlock_finished from `finally` — on
        // success, error, and cancel alike
        bot.emit_event(BotEvent::CollectBlockFinished);
        result
    }

    /// The find → goto → equip → dig → vacuum loop. `generation` is the
    /// cancellation handle: once it goes stale the run wraps up like
    /// upstream's `PathStopped` catch — returning the partial haul, no
    /// error.
    async fn run(
        &self,
        target: &mut CollectTarget,
        world: &WorldRef,
        generation: u64,
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
            if stale(&bot.client, generation) {
                cancelled = true;
                break;
            }
            if let Err(e) = self
                .empty_inventory_if_full(chest_locations, item_filter)
                .await
            {
                if stale(&bot.client, generation) {
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
            bot.emit_event(BotEvent::CollectBlockFound(block.clone()));
            let pf = bot.pathfinder();
            // upstream's mineBlock paths with `GoalLookAtBlock` — get
            // within sight+reach of a face, not strictly adjacent
            // (`cb.ts:24`)
            match tokio::time::timeout(GOTO_TIMEOUT, pf.goto(Goal::LookAtBlock(pos))).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if stale(&bot.client, generation) {
                        cancelled = true;
                        break;
                    }
                    if !ignore_no_path {
                        return Err(e);
                    }
                    tried.insert(pos);
                    last_err = Some(e);
                    continue;
                }
                Err(_) => {
                    // a stalled path stays installed — stop it before the
                    // next candidate or `goto` sees the old goal
                    bot.pathfinder().stop();
                    if stale(&bot.client, generation) {
                        cancelled = true;
                        break;
                    }
                    if !ignore_no_path {
                        return Err(Error::Failed(format!(
                            "path to {pos} timed out after {GOTO_TIMEOUT:?}"
                        )));
                    }
                    tried.insert(pos);
                    last_err = Some(Error::Failed(format!(
                        "path to {pos} timed out after {GOTO_TIMEOUT:?}"
                    )));
                    continue;
                }
            }
            if stale(&bot.client, generation) {
                cancelled = true;
                break;
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
            // upstream mineBlock equips the best harvest tool first and
            // skips blocks the held item can't harvest
            if let Err(e) = bot
                .tool()
                .equip_for_block_opts(
                    &block,
                    &EquipOpts {
                        require_harvest: true,
                        get_from_chest: true,
                        max_tools: Some(2),
                    },
                )
                .await
            {
                if stale(&bot.client, generation) {
                    cancelled = true;
                    break;
                }
                return Err(e);
            }
            if let Some(held) = bot.held_item()
                && !can_harvest(block.state, &held.stack)
            {
                tried.insert(pos);
                continue;
            }
            match bot.dig(&block).await {
                Ok(()) => {
                    bot.emit_event(BotEvent::Collected(block.clone()));
                    collected.push(block);
                    tried.insert(pos); // it's air now — don't re-find it
                    self.vacuum_drops(generation).await;
                }
                Err(e) => {
                    tried.insert(pos);
                    last_err = Some(e);
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
        bot.emit_event(BotEvent::CollectDone {
            collected: collected.len(),
        });
        Ok(Collected { blocks: collected })
    }

    /// `bot.collectBlock.cancelTask()` — retire every in-flight collect
    /// and wait out the `collectBlock_finished` they emit, like
    /// upstream's `once(bot, 'collectBlock_finished')` after
    /// `pathfinder.stop()`. A no-op when nothing is running.
    pub async fn cancel_task(&self) {
        let bot = self.bot;
        // subscribe before the state check so a run finishing between
        // the two can't lose its `finished` event
        let mut rx = bot.subscribe();
        let active = collect_state_mut(&bot.client, |s| {
            if s.active == 0 {
                return false;
            }
            s.generation += 1;
            true
        })
        .unwrap_or(false);
        if !active {
            return;
        }
        // upstream `bot.pathfinder.stop()` — wakes a parked goto so the
        // loop can see its stale generation promptly
        bot.pathfinder().stop();
        let mut shutdown = bot.shutdown.clone();
        let _ = shutdown.borrow_and_update();
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(BotEvent::CollectBlockFinished) => return,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = shutdown.changed() => return,
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
    async fn vacuum_drops(&self, generation: u64) {
        // drops spawn a tick or two after the break lands server-side
        let _ = self.bot.wait_for_ticks(2).await;
        for _ in 0..MAX_VACUUM_ATTEMPTS {
            if stale(&self.bot.client, generation) {
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
    world: &mineflayer_engine::world::World,
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
mod tests {
    use mineflayer_engine::{
        block::BlockState,
        registry::builtin::BlockKind,
        world::{Chunk, PartialChunkStorage},
    };

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

    #[test]
    fn collect_opts_defaults_match_upstream() {
        let opts = CollectOpts::default();
        assert!(!opts.append);
        assert!(!opts.ignore_no_path);
        assert!(opts.chest_locations.is_none());
        assert!(opts.item_filter.is_none());
    }

    /// Upstream's default `itemFilter` rejects tools/weapons/armor and
    /// keeps everything else.
    #[test]
    fn default_item_filter_keeps_loot_and_holds_gear() {
        let filter = default_item_filter();
        let item = |kind: mineflayer_engine::registry::builtin::ItemKind| Item {
            stack: mineflayer_engine::inventory::ItemStack::new(kind, 1),
            slot: 0,
        };
        use mineflayer_engine::registry::builtin::ItemKind;
        assert!(!filter(&item(ItemKind::DiamondPickaxe)));
        assert!(!filter(&item(ItemKind::DiamondSword)));
        assert!(!filter(&item(ItemKind::IronChestplate)));
        assert!(!filter(&item(ItemKind::Shield)));
        assert!(filter(&item(ItemKind::Cobblestone)));
        assert!(filter(&item(ItemKind::OakLog)));
    }

    #[test]
    fn plugin_state_round_trip() {
        let bot = Bot::new(despawned_client());
        // despawned clients can't hold state — accessors fall back to
        // defaults rather than panicking
        assert!(bot.collect_block().chest_locations().is_empty());
        assert!(bot.collect_block().movements().is_some());
        let filter = bot.collect_block().item_filter();
        let pick = Item {
            stack: mineflayer_engine::inventory::ItemStack::new(
                mineflayer_engine::registry::builtin::ItemKind::DiamondPickaxe,
                1,
            ),
            slot: 0,
        };
        assert!(!filter(&pick));
    }

    /// A 3×3-chunk `World` like world.rs's `test_world`.
    fn vein_world() -> (mineflayer_engine::world::World, PartialChunkStorage) {
        let mut world = mineflayer_engine::world::World::default();
        let mut partial = PartialChunkStorage::default();
        for x in -1..=1 {
            for z in -1..=1 {
                partial.set(
                    &mineflayer_engine::core::position::ChunkPos { x, z },
                    Some(Chunk::default()),
                    &mut world.chunks,
                );
            }
        }
        (world, partial)
    }

    #[test]
    fn find_from_vein_floods_same_kind_neighbors() {
        let (world, _partial) = vein_world();
        let ore = BlockState::from(BlockKind::DiamondOre);
        // an L-shaped vein: (0,64,0) → (1,64,0) → (1,64,1), plus one
        // isolated ore the fill must not reach
        for pos in [
            BlockPos::new(0, 64, 0),
            BlockPos::new(1, 64, 0),
            BlockPos::new(1, 64, 1),
            BlockPos::new(10, 64, 10),
        ] {
            world.chunks.set_block_state(pos, ore);
        }
        world
            .chunks
            .set_block_state(BlockPos::new(2, 64, 2), BlockState::from(BlockKind::Stone));
        let found = vein_positions(
            &world,
            BlockPos::new(0, 64, 0),
            BlockKind::DiamondOre,
            100,
            16.,
            1,
        );
        assert_eq!(found.len(), 3);
        assert!(found.contains(&BlockPos::new(1, 64, 1)));
        assert!(!found.contains(&BlockPos::new(10, 64, 10)));
    }

    #[test]
    fn find_from_vein_respects_caps() {
        let (world, _partial) = vein_world();
        let ore = BlockState::from(BlockKind::CoalOre);
        // a line of 10 ores
        for x in 0..10 {
            world.chunks.set_block_state(BlockPos::new(x, 64, 0), ore);
        }
        // maxBlocks caps the flood
        let found = vein_positions(
            &world,
            BlockPos::new(0, 64, 0),
            BlockKind::CoalOre,
            3,
            16.,
            1,
        );
        assert_eq!(found.len(), 3);
        // manhattan maxDistance bounds the reach
        let found = vein_positions(
            &world,
            BlockPos::new(0, 64, 0),
            BlockKind::CoalOre,
            100,
            3.,
            1,
        );
        assert!(found.len() <= 4);
        assert!(!found.contains(&BlockPos::new(9, 64, 0)));
    }

    /// `cancel_task` with nothing in flight is a plain no-op — the
    /// upstream `targets.empty` early return.
    #[tokio::test]
    async fn cancel_task_without_a_run_is_a_noop() {
        let bot = Bot::new(despawned_client());
        tokio::time::timeout(Duration::from_secs(2), bot.collect_block().cancel_task())
            .await
            .expect("idle cancel_task hung");
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
