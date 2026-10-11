//! The mineflayer-pathfinder plugin surface: `bot.pathfinder()` with
//! `goto`/`setGoal`/`stop`, the `Goal*` family mapped onto azalea goals, a
//! `Movements` config, and the `path_update`/`goal_reached`/`goal_updated`/
//! `path_stop`/`path_reset` events.
//!
//! azalea's pathfinder is goal-driven: a `GotoEvent` stores
//! `Arc<dyn Goal>` + [`PathfinderOpts`] on the `Pathfinder` component and its
//! systems repath on obstruction, timeout, and partial-path completion while
//! the goal stands — the same recovery loop upstream drives by re-running
//! A* after `path_reset`, so mid-path fixes come free. Facade bookkeeping
//! azalea has no slot for (the [`Goal`] enum the user set, `dynamic`,
//! `Movements`, watcher cancellation) lives in the `PathfinderFacade` ECS
//! component on the client entity.
//!
//! Events are a tick-aligned diff of the `Pathfinder`/`ExecutingPath`/
//! `PathfinderFacade` components — azalea's `PathFoundEvent`/
//! `StopPathfindingEvent` are ECS messages that never reach the `Event`
//! stream, so the bridge diffs once per `Tick` (the same cadence upstream's
//! `physicsTick` monitor uses):
//!
//! - `path_update` — a path was installed, or a calculation finished
//!   (azalea clears `is_calculating` for every result, failures included,
//!   matching upstream's per-result `path_update`).
//! - `goal_reached` — azalea cleared the goal while its success check
//!   holds (suppressed for `dynamic` goals, like upstream).
//! - `path_stop` — the goal was abandoned without reaching (emitted by
//!   `stop()`, by a follow-goal going invalid, or here for azalea-internal
//!   clears like world changes; deduped via the facade goal).
//! - `goal_updated` / `path_reset` — emitted synchronously by
//!   `set_goal`/`set_movements` at upstream's call sites.
//!
//! azalea's own repaths (patch/timeout/partial-end) surface only as
//! `path_update`s — upstream's per-reason `path_reset`s (`stuck`,
//! `block_updated`, `chunk_loaded`, …) aren't distinguishable from outside
//! the runner and are a documented `diverges`.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
    time::{Duration, Instant},
};

use azalea::{
    BlockPos, EntityRef, Event, Vec3, ecs as bevy_ecs,
    ecs::prelude::Component,
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, Position, inventory::Inventory,
        mining::get_mine_progress,
    },
    pathfinder::{
        self, CalculatePathCtx, ExecutingPath, PathfinderClientExt, PathfinderOpts, calculate_path,
        custom_state::CustomPathfinderState,
        goals::{self, Goal as AzaleaGoal},
        mining::MiningCache,
        moves, player_pos_to_block_pos,
        positions::RelBlockPos,
    },
    registry::builtin::BlockKind,
    world::ChunkStorage,
};
use tokio::sync::{broadcast, watch};

use crate::{Bot, BotEvent, Error, Result, windows::Item, world::Block};

/// Cost added to an edge that enters a `blocks_to_avoid` block — the same
/// 100 upstream's `Movements` uses for an unsafe step.
const AVOID_STEP_COST: f32 = 100.0;

/// `bot.pathfinder.thinkTimeout` upstream (5000 ms) — the per-search time
/// budget; azalea's `PathfinderOpts::max_timeout` plays the same role.
const DEFAULT_THINK_TIMEOUT: Duration = Duration::from_secs(5);

/// Upstream's `results.status` on `path_update` — the A* outcome a search
/// produced (`pf-goto.js` reads it to settle a pending `goto`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStatus {
    /// `'success'` — the path reaches the goal.
    Success,
    /// `'partial'` — only a partial path was found; the bot walks it and
    /// the search continues near the end (upstream keeps the `goto`
    /// pending on this status).
    Partial,
    /// `'noPath'` — the search space was exhausted inside the budget; no
    /// route exists.
    NoPath,
    /// `'timeout'` — the search hit its time budget (`think_timeout`)
    /// without a complete or partial path.
    Timeout,
}

/// The named errors upstream's `goto` puts on `err.name` (`pf-goto.js`)
/// — carried as variants so callers can discriminate instead of parsing
/// a message. Reached via [`Error::Path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// `err.name === 'NoPath'` — the search found no route to the goal.
    #[error("NoPath: no path to the goal!")]
    NoPath,
    /// `err.name === 'Timeout'` — the search hit the think timeout.
    #[error("Timeout: took too long to decide path to goal!")]
    Timeout,
    /// `err.name === 'GoalChanged'` — the goal was replaced before it
    /// could be completed.
    #[error("GoalChanged: the goal was changed before it could be completed!")]
    GoalChanged,
    /// `err.name === 'PathStopped'` — the path was stopped before the
    /// goal could be reached.
    #[error("PathStopped: the path was stopped before the goal could be reached!")]
    PathStopped,
}

/// The result of one `getPathTo`/`getPathFromTo` search — upstream's
/// `results` object reduced to its observed fields (azalea computes the
/// whole search in one pass, so there are no intermediate yields).
#[derive(Clone, Debug)]
pub struct PathResult {
    /// `results.status`.
    pub status: PathStatus,
    /// `results.path` — the block positions to walk (empty when the
    /// search found nothing).
    pub path: Vec<BlockPos>,
}

/// A `bot.pathfinder` goal — upstream's `goals.Goal*` family as one sum
/// type. Each variant names the azalea goal it maps to.
#[derive(Clone, Debug)]
pub enum Goal {
    /// `goals.GoalNear(x, y, z, range)` → [`goals::RadiusGoal`].
    /// Distance is measured to block *centers* (azalea semantics — a
    /// half-block offset vs upstream's raw node coords, `diverges`).
    Near {
        /// The position to get near.
        pos: Vec3,
        /// How close is close enough (euclidean, block centers).
        radius: f32,
    },
    /// `goals.GoalBlock(x, y, z)` → [`goals::BlockPosGoal`].
    Block(BlockPos),
    /// `goals.GoalGetToBlock(x, y, z)` → [`goals::ReachBlockPosGoal`]:
    /// get to a spot the block is visible/reachable from, not onto it.
    GetToBlock(BlockPos),
    /// `goals.GoalXZ(x, z)` → [`goals::XZGoal`].
    XZ {
        /// Target x coordinate.
        x: i32,
        /// Target z coordinate.
        z: i32,
    },
    /// `goals.GoalNearXZ(x, z, range)` → [`NearXZGoal`] (azalea has no
    /// xz-radius goal; ours mirrors upstream's octagonal heuristic and
    /// squared-range `isEnd`).
    NearXZ {
        /// Target x coordinate.
        x: i32,
        /// Target z coordinate.
        z: i32,
        /// How close is close enough (euclidean, xz-plane only).
        range: f32,
    },
    /// `goals.GoalY(y)` → [`goals::YGoal`].
    Y {
        /// Target y coordinate.
        y: i32,
    },
    /// `goals.GoalLookAtBlock(pos)` → [`goals::ReachBlockPosGoal`] at the
    /// default 4.5 reach — stand where a face of the block is visible.
    /// `diverges`: azalea raycasts to the block *center* rather than
    /// enumerating visible faces, and upstream's `options.reach`/
    /// `entityHeight` aren't plumbed.
    LookAtBlock(BlockPos),
    /// `goals.GoalBreakBlock(x, y, z)` → [`goals::ReachBlockPosGoal`].
    /// Upstream's is just `GoalLookAtBlock` (it positions the bot; it
    /// does not break the block — `bot.dig` does that, same here).
    BreakBlock(BlockPos),
    /// `goals.GoalInvert(goal)` — succeeds exactly where the inner goal
    /// fails. Negating the heuristic misleads A* the way azalea's
    /// deprecated `InverseGoal` did (`diverges`).
    Invert(Box<Goal>),
    /// `goals.GoalFollow(entity, range)` — a watcher task re-issues a
    /// radius goal whenever the entity's block position changes
    /// (`diverges`: re-paths on whole-block moves rather than upstream's
    /// continuous moving goal). Like upstream, `dynamic` is the
    /// `set_goal` flag, not a property of the goal — `dynamic = false`
    /// follows until the first in-range tick, `true` follows forever.
    Follow {
        /// The entity to follow.
        entity: EntityRef,
        /// Stop this far away from it.
        range: f32,
    },
    /// `goals.GoalCompositeAny(goals)` — succeed where any child succeeds;
    /// heuristic is the child minimum (`OrGoals` semantics).
    Any(Vec<Goal>),
    /// `goals.GoalCompositeAll(goals)` — succeed where every child
    /// succeeds; heuristic is the child maximum (`AndGoals`).
    All(Vec<Goal>),
}

impl Goal {
    /// The azalea goal for pathing, or `None` for a [`Goal::Follow`]
    /// whose entity isn't loaded. A `Follow` resolves to a
    /// [`goals::RadiusGoal`] at the entity's *current* position — inside
    /// composites that's upstream's snapshot semantics (a composite never
    /// sees the entity's later moves; `diverges`). Top-level `Follow`s
    /// never reach here — [`Pathfinder::issue`] drives them with a
    /// watcher that re-paths as the entity moves. `chunks` is the bot's
    /// live chunk storage, needed only by the `ReachBlockPosGoal`-backed
    /// variants' line-of-sight check.
    fn to_azalea(&self, chunks: &ChunkStorage) -> Option<Arc<dyn AzaleaGoal>> {
        match self {
            Goal::Near { pos, radius } => Some(Arc::new(goals::RadiusGoal::new(*pos, *radius))),
            Goal::Block(pos) => Some(Arc::new(goals::BlockPosGoal(*pos))),
            Goal::GetToBlock(pos) | Goal::LookAtBlock(pos) | Goal::BreakBlock(pos) => Some(
                Arc::new(goals::ReachBlockPosGoal::new(*pos, chunks.clone())),
            ),
            Goal::XZ { x, z } => Some(Arc::new(goals::XZGoal { x: *x, z: *z })),
            Goal::NearXZ { x, z, range } => Some(Arc::new(NearXZGoal {
                x: *x,
                z: *z,
                range_sq: range * range,
            })),
            Goal::Y { y } => Some(Arc::new(goals::YGoal { y: *y })),
            Goal::Invert(inner) => inner
                .to_azalea(chunks)
                .map(|goal| Arc::new(InvertedGoal(goal)) as Arc<dyn AzaleaGoal>),
            Goal::Any(children) => children
                .iter()
                .map(|child| child.to_azalea(chunks))
                .collect::<Option<Vec<_>>>()
                .map(|goals| Arc::new(AnyGoal(goals)) as Arc<dyn AzaleaGoal>),
            Goal::All(children) => children
                .iter()
                .map(|child| child.to_azalea(chunks))
                .collect::<Option<Vec<_>>>()
                .map(|goals| Arc::new(AllGoal(goals)) as Arc<dyn AzaleaGoal>),
            Goal::Follow { entity, range } => entity
                .get_component::<Position>()
                .map(|p| Arc::new(goals::RadiusGoal::new(**p, *range)) as Arc<dyn AzaleaGoal>),
        }
    }

    /// The goal's own success check at `pos` — upstream's `goal.isEnd`.
    /// For [`Goal::Follow`] that's "within range of where the entity is
    /// now" (and unreachable while the entity isn't loaded).
    fn is_end(&self, client: &azalea::Client, pos: Vec3) -> bool {
        match self {
            Goal::Follow { entity, range } => entity
                .get_component::<Position>()
                .is_some_and(|p| p.distance_to(pos) <= f64::from(*range)),
            _ => {
                let chunks = client.world().read().chunks.clone();
                self.to_azalea(&chunks)
                    .is_some_and(|goal| goal.success(player_pos_to_block_pos(pos)))
            }
        }
    }
}

/// A shared handle on an azalea goal — the ECS stores `Arc<dyn Goal>` on
/// the `Pathfinder` component, so wrapping the `Arc` is how the facade
/// keeps an evaluable copy for `dynamic` re-pathing.
#[derive(Debug)]
struct SharedGoal(Arc<dyn AzaleaGoal>);
impl AzaleaGoal for SharedGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        self.0.heuristic(n)
    }
    fn success(&self, n: BlockPos) -> bool {
        self.0.success(n)
    }
}

/// `GoalCompositeAny` — azalea's `OrGoals` needs homogeneous children;
/// ours are boxed trait objects.
#[derive(Debug)]
struct AnyGoal(Vec<Arc<dyn AzaleaGoal>>);
impl AzaleaGoal for AnyGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        self.0
            .iter()
            .map(|goal| goal.heuristic(n))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(f32::INFINITY)
    }
    fn success(&self, n: BlockPos) -> bool {
        self.0.iter().any(|goal| goal.success(n))
    }
}

/// `GoalCompositeAll` — the `AndGoals` analog.
#[derive(Debug)]
struct AllGoal(Vec<Arc<dyn AzaleaGoal>>);
impl AzaleaGoal for AllGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        self.0
            .iter()
            .map(|goal| goal.heuristic(n))
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(f32::INFINITY)
    }
    fn success(&self, n: BlockPos) -> bool {
        self.0.iter().all(|goal| goal.success(n))
    }
}

/// `GoalInvert` — the semantics of azalea's deprecated `InverseGoal`:
/// negated heuristic, negated success.
#[derive(Debug)]
struct InvertedGoal(Arc<dyn AzaleaGoal>);
impl AzaleaGoal for InvertedGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        -self.0.heuristic(n)
    }
    fn success(&self, n: BlockPos) -> bool {
        !self.0.success(n)
    }
}

/// `GoalNearXZ` — azalea has no xz-radius goal. Heuristic is upstream's
/// `distanceXZ` (octagonal: `|dx-dz| + min(dx,dz)·√2`), scaled by azalea's
/// `COST_HEURISTIC` like `XZGoal`; `isEnd` is upstream's `dx²+dz² ≤
/// range²`.
#[derive(Debug)]
struct NearXZGoal {
    x: i32,
    z: i32,
    range_sq: f32,
}
impl AzaleaGoal for NearXZGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        let x = (self.x - n.x).unsigned_abs() as f32;
        let z = (self.z - n.z).unsigned_abs() as f32;
        let (straight, diagonal) = if x < z { (z - x, x) } else { (x - z, z) };
        (diagonal * std::f32::consts::SQRT_2 + straight) * pathfinder::costs::COST_HEURISTIC
    }
    fn success(&self, n: BlockPos) -> bool {
        let dx = (self.x - n.x) as f32;
        let dz = (self.z - n.z) as f32;
        dx * dx + dz * dz <= self.range_sq
    }
}

/// `bot.pathfinder.movements` — upstream's `Movements` config.
///
/// Fields azalea 0.16 can honor are applied through [`PathfinderOpts`]
/// plus `CustomPathfinderState`; the rest are accepted for drop-in parity
/// and documented as unhonored (`diverges` rows in api-parity.md).
/// Placement knobs (`scafoldingBlocks`, `canPlaceOn`, `allow1by1towers`,
/// `exclusionAreas*`) are absent entirely: azalea never places blocks
/// while pathing, so they'd be dead config.
#[derive(Clone, Debug)]
pub struct Movements {
    /// `canDig` (default `true`) → `PathfinderOpts::allow_mining`.
    pub can_dig: bool,
    /// `canOpenDoors` (default `false`). Unhonored: azalea never opens
    /// doors or gates — closed ones are simply impassable.
    pub can_open_doors: bool,
    /// `allowParkour` (default `true`) → `moves::parkour` edges are
    /// generated.
    pub allow_parkour: bool,
    /// `allowSprinting` (default `true`). Unhonored: azalea picks
    /// walk/sprint inside each edge's executor; there is no global
    /// toggle.
    pub allow_sprinting: bool,
    /// `allowFreeMotion` (default `false`). Unhonored: upstream's
    /// fly-straight-to-goal shortcut has no azalea analog.
    pub allow_free_motion: bool,
    /// `dontCreateFlow` (default `true`). Unhonored: azalea doesn't model
    /// liquid flow released by mining.
    pub dont_create_flow: bool,
    /// `dontMineUnderFallingBlock` (default `true`). Unhonored: azalea's
    /// break costs don't special-case gravity blocks.
    pub dont_mine_under_falling_block: bool,
    /// `maxDropDown` (default `4`). Unhonored: azalea bounds falls by
    /// cost constants, not an exposed cap.
    pub max_drop_down: u32,
    /// `blocksToAvoid` (default `{fire, cobweb, lava}`) — edges entering
    /// one get +100 cost, matching upstream's unsafe-step penalty.
    pub blocks_to_avoid: HashSet<BlockKind>,
    /// `blocksCantBreak` (default `{chest}`; upstream also fills in every
    /// undiggable, which azalea already refuses to mine) — edges that
    /// would have to mine through one are dropped. Approximation: breaks
    /// hidden inside a multi-block move aren't visible to the filter.
    pub blocks_cant_break: HashSet<BlockKind>,
}

impl Default for Movements {
    fn default() -> Self {
        Self {
            can_dig: true,
            can_open_doors: false,
            allow_parkour: true,
            allow_sprinting: true,
            allow_free_motion: false,
            dont_create_flow: true,
            dont_mine_under_falling_block: true,
            max_drop_down: 4,
            blocks_to_avoid: HashSet::from([BlockKind::Fire, BlockKind::Cobweb, BlockKind::Lava]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
        }
    }
}

impl Movements {
    /// The [`PathfinderOpts`] for this config — `can_dig` maps onto
    /// `allow_mining`, and every goto runs [`movements_move`] so the
    /// avoid/cant-break/parkour constraints reach the search.
    ///
    /// `retry_on_no_path: false` matches upstream: one failed search is a
    /// terminal `path_update` (`noPath`/`timeout`), not an infinite retry.
    /// azalea's per-tick `recalculate_if_has_goal_but_no_path` would
    /// otherwise loop 5 s searches against unreachable goals forever and
    /// a `goto` could never reject — the goal drop is what lets the
    /// bridge report the failure (see [`bridge_events`]).
    fn to_opts(&self) -> PathfinderOpts {
        PathfinderOpts::new()
            .successors_fn(movements_move)
            .allow_mining(self.can_dig)
            .retry_on_no_path(false)
    }
}

/// The `Movements` subset a successors fn can act on, read back through
/// azalea's `CustomPathfinderState` typemap — `SuccessorsFn` is a plain
/// `fn` that can't capture config, but `MovesCtx::custom_state` is handed
/// to it by the pathfinder itself.
#[derive(Clone)]
struct MovementConstraints {
    allow_parkour: bool,
    blocks_to_avoid: Arc<HashSet<BlockKind>>,
    blocks_cant_break: Arc<HashSet<BlockKind>>,
}

impl From<&Movements> for MovementConstraints {
    fn from(m: &Movements) -> Self {
        Self {
            allow_parkour: m.allow_parkour,
            blocks_to_avoid: Arc::new(m.blocks_to_avoid.clone()),
            blocks_cant_break: Arc::new(m.blocks_cant_break.clone()),
        }
    }
}

/// The successors fn every facade goto uses: azalea's `default_move` set
/// minus parkour when disabled, with `blocks_to_avoid`/`blocks_cant_break`
/// applied to the produced edges. With no constraints installed it's
/// exactly `default_move` (the facade always installs them, so this is
/// only a safe default for raw-azalea callers).
fn movements_move(ctx: &mut moves::MovesCtx, node: RelBlockPos) {
    let Some(constraints) = ctx.custom_state.get::<MovementConstraints>() else {
        moves::default_move(ctx, node);
        return;
    };
    moves::basic::basic_move(ctx, node);
    if constraints.allow_parkour {
        moves::parkour::parkour_move(ctx, node);
    }
    moves::uncommon::uncommon_move(ctx, node);

    if constraints.blocks_to_avoid.is_empty() && constraints.blocks_cant_break.is_empty() {
        return;
    }
    let world = ctx.world;
    ctx.edges.retain_mut(|edge| {
        // occupying `target` needs both the feet and head block free
        let feet = world.get_block_state(edge.movement.target);
        if cant_break(&constraints.blocks_cant_break, feet) {
            return false;
        }
        let head = world.get_block_state(edge.movement.target.up(1));
        if cant_break(&constraints.blocks_cant_break, head) {
            return false;
        }
        if constraints.blocks_to_avoid.contains(&BlockKind::from(feet)) {
            edge.cost += AVOID_STEP_COST;
        }
        true
    });
}

/// Whether moving onto `state` would mean breaking a `blocks_cant_break`
/// member: azalea wants to mine through it (solid and diggable) but the
/// config forbids it.
fn cant_break(cant_break: &HashSet<BlockKind>, state: azalea::block::BlockState) -> bool {
    !cant_break.is_empty()
        && moves::should_mine_block_state(state)
        && cant_break.contains(&BlockKind::from(state))
}

/// Facade state azalea has no slot for: the [`Goal`] the user set (azalea
/// keeps only the opaque `Arc<dyn Goal>`), the `dynamic` flag, the
/// `Movements`, and the cancel flag for the follow/dynamic watcher.
#[derive(Component)]
pub(crate) struct PathfinderFacade {
    /// The last `set_goal` argument. `None` once the goal is reached or
    /// abandoned (cleared by the bridge diff, `stop`, or a watcher exit).
    pub goal: Option<Goal>,
    /// Bumped every `set_goal` and every `set_movements` re-issue — lets
    /// the bridge tell "goal was replaced" from "goal was abandoned" when
    /// it would otherwise clobber a new goal with a stale clear, and lets
    /// watchers tell "my issue is still current" from "a newer goal or
    /// movements config superseded it".
    pub goal_gen: u64,
    /// Bumped by `set_goal` only — `goto` waits compare it to learn "the
    /// goal was replaced" (`GoalChanged`) even when the `goal_updated`
    /// event lagged out of the broadcast window. `set_movements`
    /// re-issues don't count: upstream emits `path_reset` there, not
    /// `goal_updated`, so a pending goto must survive it.
    pub set_gen: u64,
    /// The `setGoal` `dynamic` argument — suppresses `goal_reached` and
    /// re-paths whenever the goal stops holding (upstream semantics).
    pub dynamic: bool,
    /// `bot.pathfinder.movements`.
    pub movements: Movements,
    /// `bot.pathfinder.thinkTimeout` — the per-search budget, applied as
    /// `PathfinderOpts::max_timeout` at issue time; the bridge compares a
    /// failed search's duration against it to report `NoPath` vs
    /// `Timeout`.
    pub think_timeout: Duration,
    /// The terminal search outcome that cleared `goal`, if any — lets a
    /// `goto` ticking in just after the failure-clear still reject
    /// `NoPath`/`Timeout` instead of the generic `PathStopped`.
    pub last_failure: Option<PathError>,
    /// Set to cancel the running follow/dynamic watcher.
    watcher_cancel: Option<Arc<AtomicBool>>,
}

impl Default for PathfinderFacade {
    fn default() -> Self {
        Self {
            goal: None,
            goal_gen: 0,
            set_gen: 0,
            dynamic: false,
            movements: Movements::default(),
            think_timeout: DEFAULT_THINK_TIMEOUT,
            last_failure: None,
            watcher_cancel: None,
        }
    }
}

/// Run `f` against the [`PathfinderFacade`] component, inserting a default
/// one first if the client entity lacks it. `None` when the entity is gone.
fn facade_mut<R>(client: &azalea::Client, f: impl FnOnce(&mut PathfinderFacade) -> R) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    if entity.get::<PathfinderFacade>().is_none() {
        entity.insert(PathfinderFacade::default());
    }
    entity.get_mut::<PathfinderFacade>().map(|mut c| f(&mut c))
}

/// Read from the [`PathfinderFacade`] component. `None` when it or the
/// entity doesn't exist yet.
fn facade<R>(client: &azalea::Client, f: impl FnOnce(&PathfinderFacade) -> R) -> Option<R> {
    let ecs = client.ecs.read();
    let entity = ecs.get_entity(client.entity).ok()?;
    entity.get::<PathfinderFacade>().map(f)
}

/// Publish the `Movements` constraints into `CustomPathfinderState` so
/// [`movements_move`] sees them during the next search.
fn sync_constraints(client: &azalea::Client, movements: &Movements) {
    let mut ecs = client.ecs.write();
    let Ok(mut entity) = ecs.get_entity_mut(client.entity) else {
        return;
    };
    let constraints = MovementConstraints::from(movements);
    if let Some(state) = entity.get::<CustomPathfinderState>() {
        state.0.write().insert(constraints);
    } else {
        let state = CustomPathfinderState::default();
        state.0.write().insert(constraints);
        entity.insert(state);
    }
}

/// The `bot.pathfinder` namespace — a borrowed view onto the [`Bot`],
/// like [`Bot::creative`]'s `Creative`.
pub struct Pathfinder<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.pathfinder` — the mineflayer-pathfinder namespace (`goto`,
    /// `set_goal`, `stop`, `movements`, the `Goal` family).
    pub fn pathfinder(&self) -> Pathfinder<'_> {
        Pathfinder { bot: self }
    }
}

impl Pathfinder<'_> {
    fn client(&self) -> &azalea::Client {
        &self.bot.client
    }

    /// `bot.pathfinder.goto(goal)` — set the goal and resolve when it's
    /// reached; errors on `path_stop` (PathStopped), a failed search
    /// (NoPath/Timeout, matching upstream's `results.status` handling in
    /// `pf-goto.js`), a goal replacement (GoalChanged), or disconnect.
    /// (`dynamic` goals are set via [`Pathfinder::set_goal`].)
    ///
    /// Events are the fast path; the tick poll is the source of truth —
    /// the event stream floods with packets on a busy world, and a lagged
    /// receiver could skip the one event this future waits on. The
    /// `set_gen`/`last_failure` facade fields give the poll the same
    /// discrimination the events carry.
    pub async fn goto(&self, goal: Goal) -> Result<()> {
        // subscribe before set_goal so an already-satisfied goal can't
        // emit `GoalReached` before we listen
        let mut rx = self.bot.events();
        let mut ticks = self.client().get_tick_broadcaster();
        // `End` arrives over `rx`, but >8192 queued events at disconnect
        // can push it past a lagged receiver — the bridge's shutdown watch
        // closes that gap
        let mut shutdown = self.bot.shutdown.clone();
        let _ = shutdown.borrow_and_update();
        let client = self.client().clone();
        let probe = goal.clone();
        self.set_goal(goal, false)?;
        let my_gen = facade(&client, |f| f.set_gen).unwrap_or_default();
        loop {
            tokio::select! {
                biased;
                r = rx.recv() => {
                    match r {
                        Ok(BotEvent::GoalReached { .. }) => return Ok(()),
                        Ok(BotEvent::PathUpdate { status, .. }) => {
                            // a terminal update settles this goto only
                            // when the bridge recorded OUR failure — goal
                            // cleared, `set_gen` still ours, and
                            // `last_failure` carrying the same outcome.
                            // Otherwise it's stale (a previous goal's
                            // update, or an aborted calc's `noPath`
                            // report racing a `path_stop`) — skip it.
                            if matches!(status, PathStatus::NoPath | PathStatus::Timeout)
                                && let Some(err) = facade(&client, |f| {
                                    if f.goal.is_none() && f.set_gen == my_gen {
                                        f.last_failure
                                    } else {
                                        None
                                    }
                                })
                                .flatten()
                            {
                                return Err(err.into());
                            }
                        }
                        Ok(BotEvent::PathStop { .. }) => {
                            return Err(PathError::PathStopped.into());
                        }
                        Ok(BotEvent::GoalUpdated { .. }) => {
                            // a `set_goal` after ours — our own event is
                            // skipped because `set_gen` is still ours
                            if facade(&client, |f| f.set_gen) != Some(my_gen) {
                                return Err(PathError::GoalChanged.into());
                            }
                        }
                        Ok(BotEvent::End(_)) => return Err(Error::NotConnected),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(Error::NotConnected);
                        }
                    }
                }
                _ = shutdown.changed() => return Err(Error::NotConnected),
                t = ticks.recv() => {
                    if matches!(t, Err(broadcast::error::RecvError::Closed)) {
                        return Err(Error::NotConnected);
                    }
                    // a Lagged tick still means time passed — poll anyway.
                    // the facade goal is cleared on reach/stop/failure, so
                    // resolve from state, lagged events or not
                    match facade(&client, |f| {
                        (f.set_gen, f.goal.is_some(), f.last_failure)
                    }) {
                        // a `set_goal` after ours — the `goal_updated`
                        // event may never have reached us (Lagged)
                        Some((set_gen, _, _)) if set_gen != my_gen => {
                            return Err(PathError::GoalChanged.into());
                        }
                        Some((_, false, failed)) => {
                            return if probe.is_end(&client, client.position()) {
                                Ok(())
                            } else {
                                Err(failed.unwrap_or(PathError::PathStopped).into())
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// `bot.pathfinder.setGoal(goal, dynamic?)` — stores the goal and
    /// starts pathing. Emits `goal_updated` first, then `path_reset`
    /// (`goal_updated`) when a path was underway, like upstream.
    ///
    /// `dynamic` mirrors upstream: the goal keeps re-pathing whenever it
    /// stops holding instead of completing. It is purely the `setGoal`
    /// flag — upstream's `GoalFollow` is *not* implicitly dynamic, so a
    /// `Goal::Follow` with `dynamic = false` ends at the first in-range
    /// tick, and `dynamic = true` follows until stopped or despawned.
    pub fn set_goal(&self, goal: Goal, dynamic: bool) -> Result<()> {
        let client = self.client().clone();
        let movements = self.movements();
        sync_constraints(&client, &movements);
        // Already satisfied — azalea would drop the goto without ever
        // storing the goal, so mirror upstream's monitor: `goal_updated`
        // then `goal_reached`, and stop the in-flight path (upstream's
        // resetPath also cleared it).
        let satisfied = !dynamic && goal.is_end(&client, client.position());
        if satisfied {
            stop_pathfinding_patched(&client, false);
        }
        facade_mut(&client, |f| {
            cancel_watcher(f);
            f.goal = (!satisfied).then(|| goal.clone());
            f.dynamic = dynamic;
            f.goal_gen += 1;
            f.set_gen += 1;
            f.last_failure = None;
        });
        self.bot.emit_event(BotEvent::GoalUpdated {
            goal: goal.clone(),
            dynamic,
        });
        if client.is_executing_path() {
            self.bot.emit_event(BotEvent::PathReset {
                reason: "goal_updated",
            });
        }
        if satisfied {
            self.bot.emit_event(BotEvent::GoalReached { goal });
            return Ok(());
        }
        let result = self.issue(goal, dynamic, movements);
        if result.is_err() {
            facade_mut(&client, |f| f.goal = None);
        }
        result
    }

    /// Issue a goal to azalea (or its watcher task for
    /// [`Goal::Follow`]/`dynamic`) after `set_goal` has done the facade
    /// bookkeeping.
    fn issue(&self, goal: Goal, dynamic: bool, movements: Movements) -> Result<()> {
        let client = self.client().clone();
        // Watchers re-check `goal_gen` against this before every
        // re-issue — a `set_goal` since their spawn bumps it, so they
        // can never write a stale `GotoEvent` over a replacement goal
        // (azalea's `goto_listener` keeps the last event, and a stale
        // goal would later emit a spurious `path_stop`).
        let (issue_gen, think_timeout) =
            facade(&client, |f| (f.goal_gen, f.think_timeout)).unwrap_or_default();
        let opts = movements.to_opts().max_timeout(think_timeout);
        match goal {
            Goal::Follow { entity, range } => {
                let cancel = Arc::new(AtomicBool::new(false));
                // set_movements re-issues through here too — always retire
                // the previous watcher before starting a new one
                facade_mut(&client, |f| {
                    cancel_watcher(f);
                    f.watcher_cancel = Some(cancel.clone());
                });
                follow_driver(
                    client.clone(),
                    entity,
                    range,
                    opts,
                    cancel,
                    self.bot.clone(),
                    issue_gen,
                );
            }
            _ => {
                let chunks = client.world().read().chunks.clone();
                let Some(azalea_goal) = goal.to_azalea(&chunks) else {
                    return Err(Error::Operation(
                        "Goal::Follow's entity isn't loaded".to_string(),
                    ));
                };
                client.start_goto_with_opts(SharedGoal(azalea_goal.clone()), opts.clone());
                if dynamic {
                    let cancel = Arc::new(AtomicBool::new(false));
                    facade_mut(&client, |f| {
                        cancel_watcher(f);
                        f.watcher_cancel = Some(cancel.clone());
                    });
                    dynamic_watcher(
                        client.clone(),
                        azalea_goal,
                        opts,
                        cancel,
                        self.bot.shutdown.clone(),
                        issue_gen,
                    );
                }
            }
        }
        Ok(())
    }

    /// `bot.pathfinder.stop()` — stop navigating now (upstream clears the
    /// path immediately, so this is the `force` stop, not the
    /// mid-move-friendly `stop_pathfinding`). Emits `path_stop` here like
    /// upstream's `stop()`; azalea's goal clear then arrives with the
    /// facade goal already `None`, so the bridge doesn't re-report it.
    pub fn stop(&self) {
        let client = self.client();
        // upstream: `if (!stateGoal && path.length === 0) return` — an
        // idle stop emits nothing
        let active = !client.is_goto_target_reached()
            || facade(client, |f| f.goal.is_some()).unwrap_or(false);
        stop_pathfinding_patched(client, true);
        let stopped = facade_mut(client, |f| {
            cancel_watcher(f);
            f.last_failure = None;
            f.goal.take()
        })
        .flatten();
        if active {
            self.bot.emit_event(BotEvent::PathStop { goal: stopped });
        }
    }

    /// `bot.pathfinder.isMoving()` — whether a path is being executed.
    /// Like upstream's `path.length > 0`, this is false while a path is
    /// still being calculated.
    pub fn is_moving(&self) -> bool {
        self.client().is_executing_path()
    }

    /// `bot.pathfinder.isThinking()` — whether a path is currently being
    /// calculated.
    pub fn is_thinking(&self) -> bool {
        self.client().is_calculating_path()
    }

    /// `bot.pathfinder.haveGoal()` — whether a goal is currently set.
    pub fn have_goal(&self) -> bool {
        self.goal().is_some()
    }

    /// `bot.pathfinder.goal` — the goal `set_goal` last installed
    /// (upstream's `stateGoal`), `None` once it's reached or stopped.
    pub fn goal(&self) -> Option<Goal> {
        facade(self.client(), |f| f.goal.clone()).flatten()
    }

    /// `bot.pathfinder.movements` — the current `Movements` config
    /// (upstream defaults until `set_movements` runs).
    pub fn movements(&self) -> Movements {
        facade(self.client(), |f| f.movements.clone()).unwrap_or_default()
    }

    /// `bot.pathfinder.setMovements(movements)` — replaces the config and,
    /// like upstream, abandons the in-progress path (`path_reset` with
    /// `movements_updated`) and re-paths under the new movements.
    pub fn set_movements(&self, movements: Movements) {
        let client = self.client().clone();
        sync_constraints(&client, &movements);
        let was_executing = client.is_executing_path();
        let goal_and_dynamic = facade_mut(&client, |f| {
            f.movements = movements.clone();
            // re-issuing is a new goal generation — watchers still
            // holding the previous generation stop instead of
            // overwriting the fresh goto with the old movements
            f.goal_gen += 1;
            f.goal.clone().zip(Some(f.dynamic))
        })
        .flatten();
        if was_executing {
            self.bot.emit_event(BotEvent::PathReset {
                reason: "movements_updated",
            });
        }
        if let Some((goal, dynamic)) = goal_and_dynamic {
            let _ = self.issue(goal, dynamic, movements);
        }
    }

    /// `bot.pathfinder.thinkTimeout` — the per-search budget applied to
    /// every issued goto (default 5000 ms like upstream). Passed to
    /// azalea as `PathfinderOpts::max_timeout`; also the NoPath-vs-Timeout
    /// threshold the bridge classifies a failed search against.
    pub fn think_timeout(&self) -> Duration {
        facade(self.client(), |f| f.think_timeout).unwrap_or(DEFAULT_THINK_TIMEOUT)
    }

    /// `bot.pathfinder.thinkTimeout = ms` — affects goals issued from
    /// this point on (upstream mutates the live search context; ours is
    /// read at issue time).
    pub fn set_think_timeout(&self, timeout: Duration) {
        facade_mut(self.client(), |f| f.think_timeout = timeout);
    }

    /// `bot.pathfinder.isMining()` — whether the bot is currently mining
    /// a block (azalea's `Mining` component — the one the pathfinder's
    /// mine edges drive).
    pub fn is_mining(&self) -> bool {
        self.client().is_mining()
    }

    /// `bot.pathfinder.isBuilding()` — always `false`: azalea's
    /// successors never produce scaffolding placements, so the bot never
    /// builds while pathing (`diverges` — upstream can return true).
    pub fn is_building(&self) -> bool {
        false
    }

    /// `bot.pathfinder.bestHarvestTool(block)` — the inventory item that
    /// digs `block` fastest, via azalea's `get_mine_progress` (accounts
    /// for held-item rules, water on the eyes, haste/fatigue). `None`
    /// when the inventory is empty or the entity isn't loaded, like
    /// upstream's `null` for `bot.inventory.items()` being empty.
    pub fn best_harvest_tool(&self, block: &Block) -> Option<Item> {
        let client = self.client();
        let block_trait = Box::<dyn azalea::block::BlockTrait>::from(block.state);
        client
            .try_query_self::<(
                &Inventory,
                &FluidOnEyes,
                &Physics,
                &Attributes,
                &ActiveEffects,
            ), _>(|(inventory, fluid, physics, attributes, effects)| {
                // dig time is proportional to the inverse of
                // per-tick mine progress, so argmax progress ==
                // upstream's argmin digTime over `inventory.items()`
                let mut best: Option<(usize, f32)> = None;
                for (i, stack) in inventory.inventory_menu.slots().iter().enumerate() {
                    if stack.is_empty() {
                        continue;
                    }
                    let progress = get_mine_progress(
                        &*block_trait,
                        stack,
                        fluid,
                        physics,
                        attributes,
                        effects,
                    );
                    if best.is_none_or(|(_, b)| progress > b) {
                        best = Some((i, progress));
                    }
                }
                best.map(|(i, _)| (i as i16, inventory.inventory_menu.slots()[i].clone()))
            })
            .ok()
            .flatten()
            .and_then(|(slot, stack)| Item::of(stack, slot))
    }

    /// `bot.pathfinder.getPathTo(movements, goal, timeout?)` — run one
    /// A* search from the bot's current position and return the result;
    /// the goal is *not* installed and no events fire (upstream's
    /// `getPathTo` is a pure preview). `timeout` defaults to
    /// `think_timeout` like upstream's `options.timeout ?? thinkTimeout`.
    ///
    /// `diverges`: upstream returns a generator's first `{result}` and
    /// keeps an `astarContext` for follow-up computes; azalea's search
    /// is one synchronous pass, so only the first result exists.
    pub fn get_path_to(
        &self,
        movements: &Movements,
        goal: &Goal,
        timeout: Option<Duration>,
    ) -> Result<PathResult> {
        let start = player_pos_to_block_pos(self.client().position());
        self.get_path_from_to(movements, start, goal, timeout)
    }

    /// `bot.pathfinder.getPathFromTo(movements, startPos, goal,
    /// {timeout})` — one synchronous A* pass over the live world, with
    /// the same `Movements` constraints a real goto would get.
    ///
    /// `diverges` vs upstream's generator: no incremental `yield`s —
    /// callers wanting the walk continued call again from the path end.
    /// `optimizePath`/`resetEntityIntersects`/`searchRadius`/`tickTimeout`
    /// have no azalea knob and are not exposed.
    pub fn get_path_from_to(
        &self,
        movements: &Movements,
        start: BlockPos,
        goal: &Goal,
        timeout: Option<Duration>,
    ) -> Result<PathResult> {
        let client = self.client();
        let chunks = client.world().read().chunks.clone();
        let Some(azalea_goal) = goal.to_azalea(&chunks) else {
            return Err(Error::Operation(
                "Goal::Follow's entity isn't loaded".to_string(),
            ));
        };
        let timeout = timeout.unwrap_or_else(|| self.think_timeout());
        let opts = movements.to_opts().max_timeout(timeout);
        let inventory_menu = if movements.can_dig {
            client
                .get_component::<Inventory>()
                .map(|inv| inv.inventory_menu.clone())
        } else {
            None
        };
        // a preview carries the given movements' constraints in a *fresh*
        // state — publishing into the live `CustomPathfinderState` would
        // silently change an in-flight real goto's constraints
        let custom_state = CustomPathfinderState::default();
        custom_state
            .0
            .write()
            .insert(MovementConstraints::from(movements));
        let started = Instant::now();
        // a fresh goto counter: the preview is never superseded, and it
        // can't disturb a real goto's id
        let event = calculate_path(CalculatePathCtx {
            entity: client.entity,
            start,
            goal: azalea_goal,
            world_lock: client.world(),
            goto_id_atomic: Arc::new(atomic::AtomicUsize::new(0)),
            mining_cache: MiningCache::new(inventory_menu),
            custom_state,
            opts,
        })
        .ok_or_else(|| Error::Operation("path calculation superseded mid-search".to_string()))?;
        let elapsed = started.elapsed();
        let path: Vec<BlockPos> = event
            .path
            .map(|edges| edges.iter().map(|e| e.movement.target).collect())
            .unwrap_or_default();
        let status = if !event.is_partial {
            PathStatus::Success
        } else if !path.is_empty() {
            PathStatus::Partial
        } else if elapsed >= timeout {
            PathStatus::Timeout
        } else {
            PathStatus::NoPath
        };
        Ok(PathResult { status, path })
    }
}

/// Cancel the follow/dynamic watcher if one is running — the flag, not a
/// task handle, so a stale watcher can never clobber a successor's state.
fn cancel_watcher(facade: &mut PathfinderFacade) {
    if let Some(cancel) = facade.watcher_cancel.take() {
        cancel.store(true, atomic::Ordering::Relaxed);
    }
}

/// Stop azalea's pathfinder (`force` = halt mid-move) and patch the
/// component state its 0.16 stop handler drops:
/// `handle_stop_pathfinding_event` removes `ComputePath` without
/// resetting `is_calculating`, and with a calculation in flight but no
/// `ExecutingPath` its `(&mut Pathfinder, &mut ExecutingPath)` query
/// fails outright so `goal` isn't even cleared — either leaves
/// `is_goto_target_reached`/`is_thinking` (and the `goto` future on
/// them) stuck until the next goto.
///
/// Workaround for azalea 0.16 (no upstream issue filed); drop the
/// component write once a released azalea clears `is_calculating` (and
/// handles the no-`ExecutingPath` case) in
/// `handle_stop_pathfinding_event` — verify against azalea's
/// `src/pathfinder/mod.rs`.
fn stop_pathfinding_patched(client: &azalea::Client, force: bool) {
    if force {
        client.force_stop_pathfinding();
    } else {
        client.stop_pathfinding();
    }
    let _ = client.try_query_self::<&mut pathfinder::Pathfinder, _>(|mut p| {
        p.goal = None;
        p.is_calculating = false;
    });
}

/// The [`Goal::Follow`] driver: every tick, re-issue a radius goal on the
/// entity's new block position (azalea merges it into the running path).
/// Ends silently when the facade goal is gone or its generation moved on
/// — a non-dynamic follow ends once the bridge reports `goal_reached`,
/// and `stop()`/`set_goal`/`set_movements` retire it through the cancel
/// flag and `goal_gen`. Disconnect retires it through `Bot::shutdown`
/// (the tick channel alone never closes while the driver holds a
/// `Client`). The entity leaving the world mirrors upstream's `isValid()`
/// failure: the driver clears the facade goal, stops pathfinding, and
/// reports `path_stop` itself (the ECS goal may never have been
/// installed, so the diff can't see it).
fn follow_driver(
    client: azalea::Client,
    entity: EntityRef,
    range: f32,
    opts: PathfinderOpts,
    cancel: Arc<AtomicBool>,
    bot: Bot,
    issue_gen: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = client.get_tick_broadcaster();
        let mut shutdown = bot.shutdown.clone();
        // mark the initial value seen so only the sender's drop resolves
        let _ = shutdown.borrow_and_update();
        let mut last_target: Option<BlockPos> = None;
        let mut despawned = false;
        loop {
            tokio::select! {
                r = ticks.recv() => {
                    match r {
                        // capacity-1 broadcast — a scheduling stall loses
                        // ticks, not the follow; only a dead channel exits
                        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
                _ = shutdown.changed() => return,
            }
            if cancel.load(atomic::Ordering::Relaxed) {
                break;
            }
            if facade(&client, |f| f.goal.is_some() && f.goal_gen == issue_gen) != Some(true) {
                // reached (non-dynamic), stopped, or replaced — whoever
                // cleared or superseded it already reported the outcome;
                // a goal_gen check alone isn't enough: `goal.is_some()` on
                // the *new* goal would still be true
                return;
            }
            let Some(pos) = entity.get_component::<Position>().map(|p| **p) else {
                despawned = true;
                break;
            };
            let block_pos = BlockPos::from(pos);
            if last_target == Some(block_pos) {
                continue;
            }
            last_target = Some(block_pos);
            client.start_goto_with_opts(
                SharedGoal(Arc::new(goals::RadiusGoal::new(pos, range))),
                opts.clone(),
            );
        }
        if cancel.load(atomic::Ordering::Relaxed) || !despawned {
            return;
        }
        // the entity despawned — the follow goal is no longer valid.
        // Report `path_stop` only if the goal is still ours: a
        // reach/stop/replace that beat us to the facade already owns the
        // outcome (emitting again would double `path_stop`).
        let stopped = facade_mut(&client, |f| {
            if f.goal_gen == issue_gen {
                f.goal.take()
            } else {
                None
            }
        })
        .flatten();
        let Some(goal) = stopped else {
            return;
        };
        stop_pathfinding_patched(&client, false);
        bot.emit_event(BotEvent::PathStop { goal: Some(goal) });
    })
}

/// The `dynamic`-goal watcher: once azalea clears a reached goal, keep
/// re-issuing it whenever the goal's success check stops holding —
/// upstream's "dynamic goals never complete" semantics, which azalea's
/// goal-clears-on-success can't express on its own. `goal_gen` gates the
/// re-issue so a watcher can't write a stale `GotoEvent` over a goal
/// `set_goal`/`set_movements` installed after it spawned. Disconnect
/// retires it through `Bot::shutdown`, like [`follow_driver`].
fn dynamic_watcher(
    client: azalea::Client,
    goal: Arc<dyn AzaleaGoal>,
    opts: PathfinderOpts,
    cancel: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<()>,
    issue_gen: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = client.get_tick_broadcaster();
        let _ = shutdown.borrow_and_update();
        loop {
            tokio::select! {
                r = ticks.recv() => {
                    match r {
                        // capacity-1 broadcast — a scheduling stall loses
                        // ticks, not the watcher; only a dead channel exits
                        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
                _ = shutdown.changed() => return,
            }
            if cancel.load(atomic::Ordering::Relaxed) {
                break;
            }
            if !client.is_goto_target_reached() {
                continue;
            }
            // while the goal still holds, idle at it — upstream keeps
            // stateGoal set and satisfied
            if goal.success(player_pos_to_block_pos(client.position())) {
                continue;
            }
            if facade(&client, |f| f.goal.is_some() && f.goal_gen == issue_gen) != Some(true) {
                break;
            }
            client.start_goto_with_opts(SharedGoal(goal.clone()), opts.clone());
        }
    })
}

/// Per-tick diff state for the path events — see [`bridge_events`].
#[derive(Default)]
pub(crate) struct PathTracker {
    last: Option<PathSnapshot>,
    /// When the in-flight search was first observed — a failed search's
    /// duration compared against `think_timeout` classifies it `Timeout`
    /// (the budget ran out) vs `NoPath` (the space was exhausted early).
    calc_started: Option<Instant>,
}

/// Everything the diff needs from the ECS, extracted under one read guard
/// (extract, drop, then diff — never hold the guard across another lock).
struct PathSnapshot {
    /// `Pathfinder.goal` — kept for the success check when it clears.
    ecs_goal: Option<Arc<dyn AzaleaGoal>>,
    is_calculating: bool,
    executing: bool,
    /// The active path's node list (empty when not executing).
    path: Vec<BlockPos>,
    is_partial: bool,
    /// `PathfinderFacade.goal`/`goal_gen`/`dynamic`/`think_timeout`.
    facade_goal: Option<Goal>,
    facade_gen: u64,
    dynamic: bool,
    think_timeout: Duration,
    pos: Option<Vec3>,
}

/// Called by the bridge for every azalea [`Event`]; emits the pathfinder
/// `BotEvent`s derivable from component state. Only `Tick` carries a
/// signal — see the module docs for the diff rules.
pub(crate) fn bridge_events(
    client: &azalea::Client,
    tracker: &mut PathTracker,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Tick = event else {
        return vec![];
    };
    let now = snapshot(client);
    let Some(prev) = tracker.last.replace(now) else {
        return vec![];
    };
    let now = tracker.last.as_ref().expect("just stored");
    let mut out = Vec::new();

    if !prev.is_calculating && now.is_calculating {
        tracker.calc_started = Some(Instant::now());
    }
    let calc_finished = prev.is_calculating && !now.is_calculating;
    let calc_elapsed = if calc_finished {
        tracker.calc_started.take().map(|t| t.elapsed())
    } else {
        None
    };
    let goal_cleared = prev.ecs_goal.is_some() && now.ecs_goal.is_none();
    // shared by the path_update classification and the goal-clear branch
    // — only evaluated when something actually changed, since `is_end`
    // can raycast (ReachBlockPosGoal) and shouldn't run per tick
    let satisfied = (calc_finished || goal_cleared)
        && now
            .facade_goal
            .as_ref()
            .is_some_and(|g| now.pos.is_some_and(|pos| g.is_end(client, pos)));
    // azalea drops its goal on an empty result only when
    // `retry_on_no_path` is false (every issue here sets it) — an ECS
    // goal drop in the same tick a calculation finished, with no path
    // executing and the goal unmet, is a failed search. Duration vs the
    // think budget separates `Timeout` (budget hit) from `NoPath`
    // (search space exhausted early), matching upstream's statuses.
    let search_failure = if calc_finished
        && goal_cleared
        && !now.executing
        && now.facade_goal.is_some()
        && !satisfied
    {
        Some(if calc_elapsed.is_some_and(|e| e >= now.think_timeout) {
            PathError::Timeout
        } else {
            PathError::NoPath
        })
    } else {
        None
    };

    // `path_update` — a path got installed (possibly merged mid-run by a
    // repath) or a calculation finished. `status` mirrors upstream's
    // `results.status`.
    if (!prev.executing && now.executing) || calc_finished {
        let status = if now.executing {
            if now.is_partial {
                PathStatus::Partial
            } else {
                PathStatus::Success
            }
        } else if let Some(err) = search_failure {
            match err {
                PathError::Timeout => PathStatus::Timeout,
                _ => PathStatus::NoPath,
            }
        } else if satisfied {
            // `'success'` with an empty path — upstream resolves the
            // goto on this ("goal is already met")
            PathStatus::Success
        } else {
            // an aborted calculation (stop()/replacement mid-search):
            // upstream has no 'aborted' status — `noPath` is the closest
            // "produced nothing" bucket, and a pending `goto`
            // corroborates against `last_failure` before trusting it
            PathStatus::NoPath
        };
        out.push(BotEvent::PathUpdate {
            path: now.path.clone(),
            is_partial: now.is_partial,
            status,
        });
    }

    // `goal_reached` / `path_stop` / failure-clear — azalea cleared its
    // goal. `now.facade_goal.is_some()` gates the whole thing: `stop()`
    // and the follow driver clear the facade goal when they report, so a
    // clear arriving with it already `None` was already announced.
    // Satisfaction is checked against the *current* facade goal
    // (`is_end`), which also covers a replaced goal that
    // instant-satisfied within the same tick.
    if goal_cleared && now.facade_goal.is_some() {
        // Both branches take the facade goal only when its generation is
        // unchanged since the snapshot — a `set_goal` that landed in
        // between already announced the old goal's death via
        // `goal_updated`/`path_reset` and owns the new one.
        if satisfied && !now.dynamic {
            let reached = facade_mut(client, |f| {
                if f.goal_gen == now.facade_gen {
                    f.goal.take()
                } else {
                    None
                }
            })
            .flatten();
            if let Some(goal) = reached {
                out.push(BotEvent::GoalReached { goal });
            }
        } else if !satisfied && now.facade_gen == prev.facade_gen {
            if search_failure.is_some() {
                // failed search — clear through the same path `stop()`
                // uses so no is_calculating/goal state wedges, record the
                // outcome for pending gotos, and retire the watcher so a
                // `dynamic` goal can't resurrect the dead search into a
                // busy re-path loop. No `path_stop`: like upstream, the
                // `path_update` status is the whole report.
                stop_pathfinding_patched(client, false);
                facade_mut(client, |f| {
                    if f.goal_gen == now.facade_gen {
                        f.last_failure = search_failure;
                        cancel_watcher(f);
                        f.goal = None;
                    }
                });
            } else {
                // the same goal was abandoned without reaching (world
                // change, azalea-internal stop). A goal_gen bump means the
                // cleared goal belonged to an older set_goal — its death
                // was already announced by that call's path_reset, so say
                // nothing.
                let stopped = facade_mut(client, |f| {
                    if f.goal_gen == now.facade_gen {
                        f.goal.take()
                    } else {
                        None
                    }
                })
                .flatten();
                if let Some(goal) = stopped {
                    out.push(BotEvent::PathStop { goal: Some(goal) });
                }
            }
        }
        // satisfied && dynamic: the watcher owns the goal — it stays in
        // the facade and re-paths when it stops holding (no goal_reached,
        // matching upstream)
    }
    out
}

/// Read the current pathfinder components out of the ECS in one guard.
fn snapshot(client: &azalea::Client) -> PathSnapshot {
    let empty = PathSnapshot {
        ecs_goal: None,
        is_calculating: false,
        executing: false,
        path: Vec::new(),
        is_partial: false,
        facade_goal: None,
        facade_gen: 0,
        dynamic: false,
        think_timeout: DEFAULT_THINK_TIMEOUT,
        pos: None,
    };
    let ecs = client.ecs.read();
    let Ok(entity) = ecs.get_entity(client.entity) else {
        return empty;
    };
    let pf = entity.get::<pathfinder::Pathfinder>();
    let exec = entity.get::<ExecutingPath>();
    let fac = entity.get::<PathfinderFacade>();
    PathSnapshot {
        ecs_goal: pf.and_then(|p| p.goal.clone()),
        is_calculating: pf.is_some_and(|p| p.is_calculating),
        executing: exec.is_some(),
        path: exec
            .map(|e| e.path.iter().map(|edge| edge.movement.target).collect())
            .unwrap_or_default(),
        is_partial: exec.is_some_and(|e| e.is_path_partial),
        facade_goal: fac.and_then(|f| f.goal.clone()),
        facade_gen: fac.map_or(0, |f| f.goal_gen),
        dynamic: fac.is_some_and(|f| f.dynamic),
        think_timeout: fac.map_or(DEFAULT_THINK_TIMEOUT, |f| f.think_timeout),
        pos: entity.get::<Position>().map(|p| **p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each facade goal maps to an azalea goal whose `success`/`heuristic`
    /// matches upstream semantics at probe positions. `GetToBlock` needs a
    /// world for its LOS check, and `Follow` needs a live `EntityRef` —
    /// both are covered by the live test instead.
    #[test]
    fn goal_mapping_table() {
        let chunks = ChunkStorage::default();

        // GoalXZ ignores y entirely.
        let g = Goal::XZ { x: 5, z: 7 }.to_azalea(&chunks).unwrap();
        assert!(g.success(BlockPos::new(5, -60, 7)));
        assert!(g.success(BlockPos::new(5, 300, 7)));
        assert!(!g.success(BlockPos::new(5, 64, 8)));

        // GoalY ignores x/z.
        let g = Goal::Y { y: 70 }.to_azalea(&chunks).unwrap();
        assert!(g.success(BlockPos::new(1000, 70, -1000)));
        assert!(!g.success(BlockPos::new(1000, 71, -1000)));

        // GoalBlock is exact.
        let g = Goal::Block(BlockPos::new(1, 2, 3))
            .to_azalea(&chunks)
            .unwrap();
        assert!(g.success(BlockPos::new(1, 2, 3)));
        assert!(!g.success(BlockPos::new(1, 2, 4)));

        // GoalNear = euclidean radius — measured from block *centers*
        // (azalea semantics; upstream compares raw node coords, a
        // half-block offset — noted as diverges in api-parity.md).
        let g = Goal::Near {
            pos: Vec3::new(0.5, 64.5, 0.5),
            radius: 2.0,
        }
        .to_azalea(&chunks)
        .unwrap();
        assert!(g.success(BlockPos::new(1, 64, 1)));
        assert!(!g.success(BlockPos::new(2, 64, 2)));

        // Invert flips success and the heuristic's sign.
        let inner = Goal::XZ { x: 0, z: 0 };
        let inner_azalea = inner.to_azalea(&chunks).unwrap();
        let g = Goal::Invert(Box::new(inner)).to_azalea(&chunks).unwrap();
        assert!(g.success(BlockPos::new(10, 64, 10)));
        assert!(!g.success(BlockPos::new(0, 64, 0)));
        assert_eq!(
            g.heuristic(BlockPos::new(3, 64, 4)),
            -inner_azalea.heuristic(BlockPos::new(3, 64, 4))
        );

        // Any = OR, All = AND.
        let any = Goal::Any(vec![Goal::XZ { x: 5, z: 5 }, Goal::Y { y: 70 }])
            .to_azalea(&chunks)
            .unwrap();
        assert!(any.success(BlockPos::new(5, 64, 5)));
        assert!(any.success(BlockPos::new(9, 70, 9)));
        assert!(!any.success(BlockPos::new(9, 64, 9)));

        let all = Goal::All(vec![Goal::XZ { x: 5, z: 5 }, Goal::Y { y: 70 }])
            .to_azalea(&chunks)
            .unwrap();
        assert!(all.success(BlockPos::new(5, 70, 5)));
        assert!(!all.success(BlockPos::new(5, 64, 5)));
        assert!(!all.success(BlockPos::new(9, 70, 9)));
    }

    /// `Movements` → `MovementConstraints` — the successors-fn-visible
    /// subset is carried over faithfully.
    #[test]
    fn movements_constraints_mapping() {
        let m = Movements {
            allow_parkour: false,
            blocks_to_avoid: HashSet::from([BlockKind::Cactus]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
            ..Default::default()
        };
        let c = MovementConstraints::from(&m);
        assert!(!c.allow_parkour);
        assert!(c.blocks_to_avoid.contains(&BlockKind::Cactus));
        assert!(!c.blocks_to_avoid.contains(&BlockKind::Lava));
        assert!(c.blocks_cant_break.contains(&BlockKind::Chest));
    }

    /// A client whose ECS carries `TickBroadcast` (watchers park on it)
    /// plus the pathfinder message resources, so a watcher's
    /// `start_goto_with_opts` lands in `Messages<GotoEvent>` where the
    /// test can count it. No systems run — ticks arrive only via
    /// [`crate::test_util::tick_sender`].
    fn watcher_client(bundle: impl bevy_ecs::bundle::Bundle) -> azalea::Client {
        let mut app = azalea::app::App::new();
        app.add_plugins(azalea::tick_broadcast::TickBroadcastPlugin)
            .add_plugins(pathfinder::PathfinderPlugin);
        let (ecs, _run_systems, _exit_rx) = azalea::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        azalea::Client::new(entity, ecs)
    }

    /// Count of `GotoEvent`s the watcher has issued so far.
    fn goto_messages(client: &azalea::Client) -> usize {
        client
            .ecs
            .read()
            .resource::<bevy_ecs::message::Messages<pathfinder::GotoEvent>>()
            .len()
    }

    /// `TickBroadcast` is `broadcast::channel(1)` — a two-tick burst while
    /// the watcher is parked reports `Lagged(1)`, routine scheduling lag
    /// that must not kill the watcher. Only the bridge's shutdown drop
    /// retires it.
    #[tokio::test]
    async fn dynamic_watcher_survives_lagged_and_exits_on_shutdown() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            PathfinderFacade {
                goal: Some(Goal::Y { y: 70 }),
                goal_gen: 7,
                dynamic: true,
                ..Default::default()
            },
        ));
        // a goal the bot isn't standing in, so the watcher re-issues it
        let goal: Arc<dyn AzaleaGoal> = Arc::new(goals::BlockPosGoal(BlockPos::new(100, 64, 100)));
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let handle = dynamic_watcher(
            client.clone(),
            goal,
            Movements::default().to_opts(),
            Arc::new(AtomicBool::new(false)),
            shutdown_rx,
            7,
        );
        let ticks = crate::test_util::tick_sender(&client);

        // the spawned task subscribes on its first poll — spin until a
        // receiver exists, then overflow the capacity-1 channel
        while ticks.send(()).is_err() {
            tokio::task::yield_now().await;
        }
        ticks.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished(), "watcher died on Lagged");

        // a real tick still drives work: the unmet goal gets re-issued
        ticks.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished());
        assert!(goto_messages(&client) > 0, "watcher stopped re-issuing");

        drop(shutdown_tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("watcher outlived the connection")
            .unwrap();
    }

    /// Same survival contract for the follow driver: `Lagged` is routine,
    /// only shutdown (or cancel/despawn) retires it.
    #[tokio::test]
    async fn follow_driver_survives_lagged_and_exits_on_shutdown() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let target_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(5., 64., 0.)))
            .id();
        let target = client.entity_ref_for(target_entity);
        facade_mut(&client, |f| {
            f.goal = Some(Goal::Follow {
                entity: target.clone(),
                range: 2.,
            });
            f.goal_gen = 3;
        });
        let (bot, shutdown_tx) = crate::test_util::bot_with_shutdown(client.clone());
        let handle = follow_driver(
            client.clone(),
            target,
            2.,
            Movements::default().to_opts(),
            Arc::new(AtomicBool::new(false)),
            bot,
            3,
        );
        let ticks = crate::test_util::tick_sender(&client);

        // the spawned task subscribes on its first poll — spin until a
        // receiver exists, then overflow the capacity-1 channel
        while ticks.send(()).is_err() {
            tokio::task::yield_now().await;
        }
        ticks.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished(), "follow driver died on Lagged");

        // still working: the entity's new block position gets re-pathed
        ticks.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished());
        assert!(goto_messages(&client) > 0, "follow stopped re-pathing");

        drop(shutdown_tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("follow driver outlived the connection")
            .unwrap();
    }

    /// A `Goal::Follow` targeting an entity at `pos` — `is_end` reads the
    /// entity's `Position` instead of `client.world()`, so these tests
    /// never need a `WorldHolder` (no `parking_lot` dep to build one).
    fn follow_at(client: &azalea::Client, pos: Vec3) -> Goal {
        let target_entity = client.ecs.write().spawn(Position::new(pos)).id();
        Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 1.,
        }
    }

    /// Flip azalea's `Pathfinder` state like the search lifecycle does:
    /// `goal` + `is_calculating` while a search is in flight, both
    /// cleared when an empty result comes back (`retry_on_no_path=false`
    /// on every facade-issued goto).
    fn set_searching(client: &azalea::Client, searching: bool) {
        let mut ecs = client.ecs.write();
        let mut entity = ecs.get_entity_mut(client.entity).unwrap();
        let mut pf = entity.get_mut::<pathfinder::Pathfinder>().unwrap();
        pf.is_calculating = searching;
        pf.goal =
            searching.then(|| Arc::new(goals::XZGoal { x: 500, z: 0 }) as Arc<dyn AzaleaGoal>);
    }

    /// Drive `goto` until its goal is installed — `set_goal` runs on the
    /// future's first poll, so the watcher arm completes after a round.
    async fn await_install(
        goto: &mut std::pin::Pin<&mut impl std::future::Future<Output = Result<()>>>,
        client: &azalea::Client,
    ) {
        tokio::select! {
            r = &mut *goto => panic!("goto resolved before its goal was installed: {r:?}"),
            _ = async {
                while facade(client, |f| f.goal.is_none()).unwrap_or(true) {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    }

    /// A failed search must produce a `path_update` carrying the failure
    /// status (upstream's `results.status`), clear the facade goal, and
    /// record `last_failure` — with no `path_stop` (upstream only emits
    /// the status).
    #[test]
    fn bridge_failed_search_reports_nopath_and_clears_goal() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        let mut tracker = PathTracker::default();
        // baseline tick plants `last`, then the search starts
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // the search returns empty well inside the 5 s budget → noPath
        set_searching(&client, false);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::NoPath,
                    path,
                    ..
                } if path.is_empty()
            )),
            "expected a noPath path_update, got {out:?}"
        );
        assert!(
            !out.iter().any(|e| matches!(e, BotEvent::PathStop { .. })),
            "a failed search is a status, not a stop"
        );
        assert_eq!(
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::NoPath))),
            "facade goal must be cleared with the failure recorded"
        );
    }

    /// Same failure path, but with the search outliving a near-zero
    /// `think_timeout` → `Timeout`, matching upstream's
    /// `results.status === 'timeout'`.
    #[test]
    fn bridge_failed_search_past_budget_reports_timeout() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.goal_gen = 1;
            f.set_gen = 1;
            f.think_timeout = Duration::from_nanos(1);
        });
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        // let the tracked search age past the (nanosecond) budget
        std::thread::sleep(Duration::from_millis(2));

        set_searching(&client, false);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::Timeout,
                    ..
                }
            )),
            "expected a timeout path_update, got {out:?}"
        );
        assert_eq!(
            facade(&client, |f| f.last_failure),
            Some(Some(PathError::Timeout))
        );
    }

    /// A path arriving mid-run emits `path_update` with `success`/`partial`
    /// status, mirroring `results.status` on non-terminal updates.
    #[test]
    fn bridge_executing_path_reports_status() {
        use std::collections::VecDeque;

        fn noop(_: moves::ExecuteCtx) {}
        fn unreached(_: moves::IsReachedCtx) -> bool {
            false
        }
        let edge = |target| pathfinder::astar::Edge {
            movement: pathfinder::astar::Movement {
                target,
                data: moves::MoveData {
                    execute: &noop,
                    is_reached: &unreached,
                },
            },
            cost: 1.,
        };

        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
            PathfinderFacade::default(),
        ));
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // a complete path starts executing
        let mut ecs = client.ecs.write();
        ecs.entity_mut(client.entity).insert(ExecutingPath {
            path: VecDeque::from([edge(BlockPos::new(1, 64, 0))]),
            queued_path: None,
            last_reached_node: BlockPos::new(0, 64, 0),
            ticks_since_last_node_reached: 0,
            is_path_partial: false,
        });
        drop(ecs);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::Success,
                    path,
                    ..
                } if path == &vec![BlockPos::new(1, 64, 0)]
            )),
            "expected a success path_update, got {out:?}"
        );

        // a repath finishes mid-run with a partial path → `partial`
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        let mut ecs = client.ecs.write();
        {
            let mut entity = ecs.get_entity_mut(client.entity).unwrap();
            entity
                .get_mut::<pathfinder::Pathfinder>()
                .unwrap()
                .is_calculating = false;
            entity.insert(ExecutingPath {
                path: VecDeque::from([edge(BlockPos::new(2, 64, 0))]),
                queued_path: None,
                last_reached_node: BlockPos::new(0, 64, 0),
                ticks_since_last_node_reached: 0,
                is_path_partial: true,
            });
        }
        drop(ecs);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::Partial,
                    ..
                }
            )),
            "expected a partial path_update, got {out:?}"
        );
    }

    /// `goto` rejects `GoalChanged` when another `set_goal` lands while
    /// it's pending — upstream's `goal_updated` listener with
    /// `newGoal !== goal`.
    #[tokio::test]
    async fn goto_rejects_goal_changed_on_replace() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();
        let pf2 = bot.pathfinder();

        let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
        await_install(&mut goto, &client).await;

        pf2.set_goal(follow_at(&client, Vec3::new(600., 64., 0.)), false)
            .unwrap();
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
            .await
            .expect("goto ignored the goal replacement")
            .expect_err("the replaced goto resolved");
        assert!(
            matches!(err, Error::Path(PathError::GoalChanged)),
            "expected GoalChanged, got {err:?}"
        );
    }

    /// `goto` rejects `PathStopped` on `path_stop` (upstream's
    /// `err.name === 'PathStopped'`).
    #[tokio::test]
    async fn goto_rejects_path_stopped_on_stop() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();
        let pf2 = bot.pathfinder();

        let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
        await_install(&mut goto, &client).await;

        pf2.stop();
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
            .await
            .expect("goto ignored the stop")
            .expect_err("the stopped goto resolved");
        assert!(
            matches!(err, Error::Path(PathError::PathStopped)),
            "expected PathStopped, got {err:?}"
        );
    }

    /// `goto` rejects `NoPath` from a bridge-reported search failure —
    /// the tick arm surfaces `last_failure` even when the `path_update`
    /// event never arrived (the bounded wait the audit demands).
    #[tokio::test]
    async fn goto_rejects_no_path_via_tick_poll() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();

        let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
        await_install(&mut goto, &client).await;

        // play the bridge's failure outcome: goal cleared, failure
        // recorded — the goto's tick arm must surface it
        facade_mut(&client, |f| {
            f.goal = None;
            f.last_failure = Some(PathError::NoPath);
        });
        let ticks = crate::test_util::tick_sender(&client);
        while ticks.send(()).is_err() {
            tokio::task::yield_now().await;
        }
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
            .await
            .expect("goto hung on a failed search")
            .expect_err("the failed goto resolved");
        assert!(
            matches!(err, Error::Path(PathError::NoPath)),
            "expected NoPath, got {err:?}"
        );
    }

    /// `goto` resolves when the goal is already met — upstream's
    /// `status === 'success' && path.length === 0` fast path (here: the
    /// `goal_reached` `set_goal` emits when `is_end` already holds).
    #[tokio::test]
    async fn goto_resolves_on_already_satisfied_goal() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();
        // the follow target is already within range of the bot
        let goto = pf.goto(follow_at(&client, Vec3::new(0.5, 64., 0.)));
        tokio::time::timeout(std::time::Duration::from_secs(5), goto)
            .await
            .expect("goto hung on a satisfied goal")
            .expect("the satisfied goto rejected");
    }
}
