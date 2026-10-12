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
//! `physicsTick` monitor uses). Two failure classes need care beyond
//! edge detection:
//!
//! - azalea runs its whole goto chain per Update (~17 ms) while ticks
//!   land per GameTick (50 ms), so a fast-failing search can install
//!   and drop inside one gap — no `is_calculating`/goal edges survive.
//!   The bridge flags a *steady-state wedge* instead: facade goal set
//!   while the ECS shows no goal, no calc, and no path, persisting past
//!   [`WEDGE_GRACE`].
//! - upstream's `thinkTimeout` bounds one A* end-to-end, but azalea
//!   splits the search into a fresh `max_timeout` per repath, so the
//!   budget is tracked cumulatively per goal issue — partial progress
//!   can't keep a `goto` pending forever (upstream's `timeout`).
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
/// budget; azalea's `PathfinderOpts::max_timeout` plays the same role,
/// and the bridge applies it cumulatively across repaths (see
/// [`bridge_events`]) since upstream's sliced A* shares one budget.
const DEFAULT_THINK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the facade-goal-without-any-ECS-goal signature must persist
/// before it's a failed search rather than a `GotoEvent` still waiting
/// for the next Update pass to pick it up. azalea drains the goto
/// message every Update (~17 ms), so a transient wedge clears in one
/// tick; >2 GameTicks means the goal already installed and dropped
/// inside a tick gap no edge-detection could see.
const WEDGE_GRACE: Duration = Duration::from_millis(100);

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
    #[error("NoPath: No path to the goal!")]
    NoPath,
    /// `err.name === 'Timeout'` — the search hit the think timeout.
    /// (upstream's message spells it "Took to long" — kept verbatim)
    #[error("Timeout: Took to long to decide path to goal!")]
    Timeout,
    /// `err.name === 'GoalChanged'` — the goal was replaced before it
    /// could be completed.
    #[error("GoalChanged: The goal was changed before it could be completed!")]
    GoalChanged,
    /// `err.name === 'PathStopped'` — the path was stopped before the
    /// goal could be reached.
    #[error(
        "PathStopped: Path was stopped before it could be completed! \
         Thus, the desired goal was not reached."
    )]
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

    /// Whether [`Goal::to_azalea`] actually reads its `chunks`
    /// parameter — only the `ReachBlockPosGoal`-backed variants
    /// raycast into chunk storage. Lets [`Goal::is_end`] skip the
    /// `WorldHolder` lock for every other goal shape (and stay usable
    /// where no world component exists).
    fn needs_chunks(&self) -> bool {
        match self {
            Goal::GetToBlock(_) | Goal::LookAtBlock(_) | Goal::BreakBlock(_) => true,
            Goal::Invert(inner) => inner.needs_chunks(),
            Goal::Any(children) | Goal::All(children) => children.iter().any(Goal::needs_chunks),
            _ => false,
        }
    }

    /// The goal's own success check at `pos` — upstream's `goal.isEnd`.
    /// Evaluated through the azalea goal this maps to so semantics can't
    /// drift: for [`Goal::Follow`] that's the `RadiusGoal` at the
    /// entity's current position — its `success` measures from the
    /// block *center* like the installed goal does (a raw Vec3 distance
    /// here would disagree by ±0.5 per axis), and it needs no
    /// `ChunkStorage`, so this arm skips `client.world()` entirely and
    /// is `false` while the entity isn't loaded.
    fn is_end(&self, client: &azalea::Client, pos: Vec3) -> bool {
        match self {
            Goal::Follow { entity, range } => entity.get_component::<Position>().is_some_and(|p| {
                goals::RadiusGoal::new(**p, *range).success(player_pos_to_block_pos(pos))
            }),
            _ => {
                let chunks = if self.needs_chunks() {
                    client.world().read().chunks.clone()
                } else {
                    ChunkStorage::default()
                };
                self.to_azalea(&chunks)
                    .is_some_and(|goal| goal.success(player_pos_to_block_pos(pos)))
            }
        }
    }
}

/// Structural identity for the `newGoal !== goal` check upstream's
/// `goto` runs on `goal_updated` — a re-issued goal equal to the pending
/// one doesn't count as a replacement (upstream compares by object
/// identity; a value type's closest honest analog is equality, so this
/// errs toward *not* rejecting where upstream's identity check
/// wouldn't). [`Goal::Follow`] compares the target's ECS id — the
/// `Client` inside an `EntityRef` isn't part of the goal's meaning.
fn same_goal(a: &Goal, b: &Goal) -> bool {
    match (a, b) {
        (
            Goal::Near {
                pos: p1,
                radius: r1,
            },
            Goal::Near {
                pos: p2,
                radius: r2,
            },
        ) => p1 == p2 && r1 == r2,
        (Goal::Block(a), Goal::Block(b)) => a == b,
        (Goal::GetToBlock(a), Goal::GetToBlock(b)) => a == b,
        (Goal::LookAtBlock(a), Goal::LookAtBlock(b)) => a == b,
        (Goal::BreakBlock(a), Goal::BreakBlock(b)) => a == b,
        (Goal::XZ { x: x1, z: z1 }, Goal::XZ { x: x2, z: z2 }) => x1 == x2 && z1 == z2,
        (
            Goal::NearXZ {
                x: x1,
                z: z1,
                range: r1,
            },
            Goal::NearXZ {
                x: x2,
                z: z2,
                range: r2,
            },
        ) => x1 == x2 && z1 == z2 && r1 == r2,
        (Goal::Y { y: y1 }, Goal::Y { y: y2 }) => y1 == y2,
        (Goal::Invert(a), Goal::Invert(b)) => same_goal(a, b),
        (
            Goal::Follow {
                entity: e1,
                range: r1,
            },
            Goal::Follow {
                entity: e2,
                range: r2,
            },
        ) => e1.id() == e2.id() && r1 == r2,
        (Goal::Any(a), Goal::Any(b)) | (Goal::All(a), Goal::All(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_goal(x, y))
        }
        _ => false,
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
        // captured inside the set_goal facade write — a separate read
        // afterwards could adopt a racing replacement's generation and
        // never see the GoalChanged
        let my_gen = self.set_goal_inner(goal, false)?;
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
        self.set_goal_inner(goal, dynamic).map(|_| ())
    }

    /// `set_goal` that also returns the `set_gen` it established —
    /// captured inside the same facade write so a `goto` can't adopt a
    /// racing replacement's generation (the identity check's read side).
    fn set_goal_inner(&self, goal: Goal, dynamic: bool) -> Result<u64> {
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
        let set_gen = facade_mut(&client, |f| {
            cancel_watcher(f);
            // `newGoal !== goal` — upstream's identity check wouldn't
            // reject a goto when the identical goal is re-issued, so an
            // equal goal+dynamic doesn't bump `set_gen` (the re-path
            // below still happens; only the replacement flag is deduped)
            let same = f.goal.as_ref().is_some_and(|g| same_goal(g, &goal)) && f.dynamic == dynamic;
            f.goal = (!satisfied).then(|| goal.clone());
            f.dynamic = dynamic;
            f.goal_gen += 1;
            if !same {
                f.set_gen += 1;
            }
            f.last_failure = None;
            f.set_gen
        })
        .unwrap_or_default();
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
            return Ok(set_gen);
        }
        let result = self.issue(goal, dynamic, movements);
        if result.is_err() {
            facade_mut(&client, |f| f.goal = None);
        }
        result.map(|_| set_gen)
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
                // movements travel with the driver — every re-issue is
                // a fresh issue, so `think_timeout` is re-read then
                // (`set_think_timeout` must reach re-issues, M-7)
                follow_driver(
                    client.clone(),
                    entity,
                    range,
                    movements,
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
                client.start_goto_with_opts(SharedGoal(azalea_goal.clone()), opts);
                if dynamic {
                    let cancel = Arc::new(AtomicBool::new(false));
                    facade_mut(&client, |f| {
                        cancel_watcher(f);
                        f.watcher_cancel = Some(cancel.clone());
                    });
                    dynamic_watcher(
                        client.clone(),
                        azalea_goal,
                        movements,
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
    /// a block (azalea's `Mining` component). `diverges`: upstream's flag
    /// is the *pathfinder's own* `digging` state; azalea's component
    /// counts any in-progress client mining — a manual `bot.dig` in
    /// flight reports true here where upstream's would not.
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
    /// when the inventory is empty, the entity isn't loaded, or every
    /// tool's dig time is infinite (unbreakable — upstream's
    /// `digTime: Infinity` keeps `bestTool` at `null`, and zero mine
    /// progress is the same signal here). Scans `player_slots_range` —
    /// main inventory + hotbar, the range upstream's
    /// `inventory.items()` covers; crafting/armor/offhand slots are out.
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
                let menu = &inventory.inventory_menu;
                let slots = menu.slots();
                let mut best: Option<(usize, f32)> = None;
                for i in menu.player_slots_range() {
                    let stack = &slots[i];
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
                // zero progress everywhere = infinite digTime = no tool
                best.filter(|&(_, p)| p > 0.)
                    .map(|(i, _)| (i as i16, slots[i].clone()))
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
    /// The search runs on the caller thread for up to `timeout` —
    /// calling this on the bot's LocalSet thread stalls the ECS for
    /// that long, so prefer `spawn_blocking` for big searches.
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
        let status = classify_search_result(event.is_partial, path.is_empty(), elapsed, timeout);
        Ok(PathResult { status, path })
    }
}

/// Upstream's `results.status` for a finished azalea search — a
/// complete path is `success`, a non-empty partial `partial`, and the
/// empty+partial result means the space was exhausted (`noPath`) or the
/// budget ran out (`timeout`), separated by elapsed time.
fn classify_search_result(
    is_partial: bool,
    path_empty: bool,
    elapsed: Duration,
    budget: Duration,
) -> PathStatus {
    if !is_partial {
        PathStatus::Success
    } else if !path_empty {
        PathStatus::Partial
    } else if elapsed >= budget {
        PathStatus::Timeout
    } else {
        PathStatus::NoPath
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
    movements: Movements,
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
        // the goal was just issued — a drop observed on the first poll
        // is a fresh one
        let mut was_live = true;
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
            // a just-observed goal drop may be a failed search the
            // bridge hasn't cleared yet — re-issuing in that window
            // installs a stale goal nobody owns; skip this tick so the
            // failure-clear can run first
            let live = pathfinder_live(&client);
            let fresh_drop = was_live && !live;
            was_live = live;
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
            if last_target == Some(block_pos) || fresh_drop {
                continue;
            }
            last_target = Some(block_pos);
            // every issue reads the live think_timeout — a
            // `set_think_timeout` mid-follow must reach re-issues
            let think_timeout =
                facade(&client, |f| f.think_timeout).unwrap_or(DEFAULT_THINK_TIMEOUT);
            client.start_goto_with_opts(
                SharedGoal(Arc::new(goals::RadiusGoal::new(pos, range))),
                movements.to_opts().max_timeout(think_timeout),
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
    movements: Movements,
    cancel: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<()>,
    issue_gen: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = client.get_tick_broadcaster();
        let _ = shutdown.borrow_and_update();
        // the goal was just issued — a drop observed on the first poll
        // is a fresh one
        let mut was_live = true;
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
            // a just-observed goal drop may be a failed search the
            // bridge hasn't cleared yet — re-issuing in that window
            // installs a stale goal nobody owns; skip this tick so the
            // failure-clear can run first
            let live = pathfinder_live(&client);
            let fresh_drop = was_live && !live;
            was_live = live;
            if !client.is_goto_target_reached() {
                continue;
            }
            // while the goal still holds, idle at it — upstream keeps
            // stateGoal set and satisfied
            if goal.success(player_pos_to_block_pos(client.position())) {
                continue;
            }
            if fresh_drop {
                continue;
            }
            let Some(think_timeout) = facade(&client, |f| {
                (f.goal.is_some() && f.goal_gen == issue_gen).then_some(f.think_timeout)
            })
            .flatten() else {
                break;
            };
            client.start_goto_with_opts(
                SharedGoal(goal.clone()),
                movements.to_opts().max_timeout(think_timeout),
            );
        }
    })
}

/// Whether the ECS pathfinder looks live — a goal installed, a search
/// in flight, or a path executing. Watchers diff this across their own
/// ticks: a fresh drop may be a failed search whose failure-clear
/// hasn't run yet (azalea's goal drop lands in Update, the bridge sees
/// it on the next GameTick), and a re-issue inside that window would
/// install an unclaimed goal the bot then walks (M-4).
fn pathfinder_live(client: &azalea::Client) -> bool {
    client
        .try_query_self::<&pathfinder::Pathfinder, _>(|p| p.goal.is_some() || p.is_calculating)
        .unwrap_or(false)
        || client.is_executing_path()
}

/// Per-tick diff state for the path events — see [`bridge_events`].
#[derive(Default)]
pub(crate) struct PathTracker {
    last: Option<PathSnapshot>,
    /// When the in-flight search was first observed — a failed search's
    /// duration compared against `think_timeout` classifies it `Timeout`
    /// (the budget ran out) vs `NoPath` (the space was exhausted early).
    calc_started: Option<Instant>,
    /// First snapshot the goal sat wedged (facade goal set, no ECS
    /// goal/calc/path, goal unmet). A fast-failing search installs and
    /// drops inside one GameTick gap — azalea runs the whole goto chain
    /// per Update (~17 ms) while snapshots land per GameTick (50 ms) —
    /// so edge detection can't see it; the persistent wedge is the only
    /// observable signature. Armed under [`WEDGE_GRACE`] so a
    /// just-written `GotoEvent` can't false-positive.
    wedge_since: Option<Instant>,
    /// The `goal_gen` the search bookkeeping below belongs to — a
    /// `set_goal`/`set_movements` re-issue resets it (new promise, new
    /// budget).
    tracked_gen: u64,
    /// Observed search time accumulated for `tracked_gen`. Upstream's
    /// `thinkTimeout` bounds one A* end-to-end; azalea splits it into a
    /// fresh search per repath, so the budget has to span them — a goal
    /// whose searches total `think_timeout` without ever producing a
    /// complete path rejects `Timeout` even while partial progress
    /// keeps coming (the common unreachable-but-approachable case).
    search_spent: Duration,
    /// Whether `tracked_gen`'s searches have produced a complete
    /// (non-partial) path yet — once one has, the goal is established
    /// and later recalcs carry their own per-search budgets.
    saw_success: bool,
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

    // the search bookkeeping is scoped to one goal issue — a
    // `set_goal`/`set_movements` re-issue restarts the promise (fresh
    // goal, fresh budget, fresh wedge arming)
    if now.facade_gen != tracker.tracked_gen {
        tracker.tracked_gen = now.facade_gen;
        tracker.search_spent = Duration::ZERO;
        tracker.saw_success = false;
        tracker.wedge_since = None;
    }

    // a superseded calc keeps `is_calculating` true across the swap —
    // the replacement installs a new `Arc`, so a same-flag goal
    // transition means the old search was orphaned; restart the clock
    // instead of letting its elapsed bleed into the new one
    let superseded = prev.is_calculating
        && now.is_calculating
        && match (&prev.ecs_goal, &now.ecs_goal) {
            (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
            _ => false,
        };
    if (!prev.is_calculating && now.is_calculating) || superseded {
        tracker.calc_started = Some(Instant::now());
    }
    let calc_finished = prev.is_calculating && !now.is_calculating;
    let calc_elapsed = if calc_finished {
        tracker.calc_started.take().map(|t| t.elapsed())
    } else {
        None
    };
    // upstream's thinkTimeout bounds one A* end-to-end; azalea splits it
    // into a fresh search per repath, so elapsed accumulates across them
    if let Some(elapsed) = calc_elapsed {
        tracker.search_spent += elapsed;
    }
    if now.executing && !now.is_partial {
        tracker.saw_success = true;
    }
    let spent_now = tracker.search_spent
        + if now.is_calculating {
            tracker
                .calc_started
                .map(|t| t.elapsed())
                .unwrap_or_default()
        } else {
            Duration::ZERO
        };
    // the promise-level budget: searches for this goal totaling
    // think_timeout without ever producing a complete path → upstream
    // 'timeout' (azalea would repath a partial frontier forever)
    let budget_exceeded =
        now.facade_goal.is_some() && !tracker.saw_success && spent_now >= now.think_timeout;

    let goal_cleared = prev.ecs_goal.is_some() && now.ecs_goal.is_none();
    // a facade goal with no ECS goal, no calc, and no path is the only
    // signature a fast-failing search leaves when it installs and drops
    // inside one GameTick gap — the edges can't be observed
    let wedge_candidate = now.facade_goal.is_some()
        && !now.executing
        && !now.is_calculating
        && now.ecs_goal.is_none();
    // shared by the path_update classification and the goal-clear branch
    // — only evaluated when something actually changed, since `is_end`
    // can raycast (ReachBlockPosGoal) and shouldn't run per tick
    let satisfied = (calc_finished || goal_cleared || wedge_candidate || budget_exceeded)
        && now
            .facade_goal
            .as_ref()
            .is_some_and(|g| now.pos.is_some_and(|pos| g.is_end(client, pos)));

    // the wedge must persist past the grace window — a just-written
    // `GotoEvent` shows the same signature for the ~17 ms until the next
    // Update drains it, so flagging immediately would false-positive
    tracker.wedge_since = if wedge_candidate && !satisfied {
        tracker.wedge_since.or_else(|| Some(Instant::now()))
    } else {
        None
    };
    let wedged = tracker
        .wedge_since
        .is_some_and(|t| t.elapsed() >= WEDGE_GRACE);

    // azalea drops its goal on an empty result only when
    // `retry_on_no_path` is false (every issue here sets it) — an ECS
    // goal drop in the same tick a calculation finished, with no path
    // executing and the goal unmet, is a failed search; so is a wedge
    // that outlived the grace window. Duration vs the think budget
    // separates `Timeout` (budget hit) from `NoPath` (search space
    // exhausted early), matching upstream's statuses.
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
    } else if wedged {
        // a hidden install/fail/drop spent an unobservable share of the
        // budget — classify against the cumulative promise spend like
        // every other failure (Timeout only if the budget's blown)
        Some(if spent_now >= now.think_timeout {
            PathError::Timeout
        } else {
            PathError::NoPath
        })
    } else {
        None
    };
    // the cumulative promise budget expiring is also a failure — but a
    // partial path may still be walking, so unlike a dead search it
    // clears only the facade (upstream's `monitorMovement` also keeps
    // walking the best path after a 'timeout' reject)
    let promise_expired = search_failure.is_none() && budget_exceeded && !satisfied;

    // `path_update` — a path got installed (possibly merged mid-run by a
    // repath) or a calculation finished while something owned it: a
    // facade goal, or a path it just installed. `status` mirrors
    // upstream's `results.status`. A bare finish with neither — the
    // `stop()`/aborted-calc shape — stays silent, matching upstream's
    // aborted `astarContext` which produces no `path_update` at all.
    if (!prev.executing && now.executing)
        || (calc_finished && (now.executing || now.facade_goal.is_some()))
        || (search_failure.is_some() && !calc_finished)
    {
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
            // a calculation whose result landed with no path — azalea's
            // result was either empty (a real `noPath`) or superseded;
            // a pending `goto` corroborates against `last_failure`
            // before trusting it
            PathStatus::NoPath
        };
        out.push(BotEvent::PathUpdate {
            path: now.path.clone(),
            is_partial: now.is_partial,
            status,
        });
    }

    // promise-expired reject — the goal stays on azalea (it may be
    // walking a partial path; upstream keeps moving after a 'timeout'),
    // only the facade's claim on it ends. Announced via `path_update`
    // like a search-reported 'timeout'.
    if promise_expired && now.facade_gen == prev.facade_gen {
        facade_mut(client, |f| {
            if f.goal_gen == now.facade_gen {
                f.last_failure = Some(PathError::Timeout);
                cancel_watcher(f);
                f.goal = None;
            }
        });
        out.push(BotEvent::PathUpdate {
            path: now.path.clone(),
            is_partial: now.is_partial,
            status: PathStatus::Timeout,
        });
    }

    // `goal_reached` / `path_stop` / failure-clear — azalea cleared its
    // goal, or the goal sits in a wedge the drop left behind.
    // `now.facade_goal.is_some()` gates the whole thing: `stop()`
    // and the follow driver clear the facade goal when they report, so a
    // clear arriving with it already `None` was already announced.
    // Satisfaction is checked against the *current* facade goal
    // (`is_end`), which also covers a replaced goal that
    // instant-satisfied within the same tick.
    if (goal_cleared || wedge_candidate) && now.facade_goal.is_some() {
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
            } else if goal_cleared {
                // the same goal was abandoned without reaching (world
                // change, azalea-internal stop). A goal_gen bump means the
                // cleared goal belonged to an older set_goal — its death
                // was already announced by that call's path_reset, so say
                // nothing. A wedge still inside its grace window
                // (`wedge_candidate && !goal_cleared`) lands here too —
                // it gets its own verdict once the window closes.
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

        // GoalNearXZ — upstream `distanceXZ` (octagonal: straight +
        // diagonal·√2) × COST_HEURISTIC, and `isEnd` dx²+dz² ≤ range²
        // measured on integer node coords, y ignored.
        let g = Goal::NearXZ {
            x: 10,
            z: 10,
            range: 5.,
        }
        .to_azalea(&chunks)
        .unwrap();
        // dx=dz=10 → straight 0, diagonal 10 → 10·√2·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(0, 64, 0)),
            10. * std::f32::consts::SQRT_2 * pathfinder::costs::COST_HEURISTIC
        );
        // dx=6, dz=0 → straight 6, diagonal 0 → 6·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(4, 64, 10)),
            6. * pathfinder::costs::COST_HEURISTIC
        );
        // dx=6, dz=8 → diagonal 6, straight 2 → (6√2+2)·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(4, 64, 2)),
            (6. * std::f32::consts::SQRT_2 + 2.) * pathfinder::costs::COST_HEURISTIC
        );
        assert!(g.success(BlockPos::new(13, 64, 14))); // dx²+dz² = 9+16 = 25 ≤ 25
        assert!(g.success(BlockPos::new(13, 300, 14))); // y ignored
        assert!(!g.success(BlockPos::new(16, 64, 10))); // 36 > 25

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
            Movements::default(),
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
            Movements::default(),
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

    /// Same failure path, but with the search outliving the
    /// `think_timeout` budget → `Timeout`, matching upstream's
    /// `results.status === 'timeout'`. The budget is a promise-level
    /// deadline, so the reject lands mid-search — upstream's
    /// thinkTimeout aborts an in-flight A* the same way.
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
            f.think_timeout = Duration::from_millis(30);
        });
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        // let the in-flight search age past the budget — the promise
        // rejects mid-search, like upstream's thinkTimeout aborting a
        // running A*
        std::thread::sleep(Duration::from_millis(60));
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
        // the (dead) search finishing later reports nothing more —
        // the facade already recorded and cleared the outcome
        set_searching(&client, false);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
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

    /// The C-1 hole: a fast-failing search installs its goal and drops
    /// it entirely inside one GameTick gap — no `is_calculating`/goal
    /// edges ever appear. The steady-state wedge (facade goal set, ECS
    /// idle, goal unmet) past the grace window is the only signature —
    /// it must report `noPath` and clear the facade goal like an
    /// observed failure.
    #[test]
    fn bridge_wedged_goal_after_hidden_drop_reports_failure() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        // `follow_at` spawns an ECS entity — build it before the
        // facade write, not inside the `ecs.write()` guard
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        let mut tracker = PathTracker::default();
        // baseline plants `last`; the wedge arms on the next tick and
        // must NOT fire inside the grace window
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        assert!(
            bridge_events(&client, &mut tracker, &Event::Tick).is_empty(),
            "wedge fired before the grace window closed"
        );
        std::thread::sleep(WEDGE_GRACE + Duration::from_millis(30));

        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::NoPath,
                    ..
                }
            )),
            "expected a noPath path_update, got {out:?}"
        );
        assert!(!out.iter().any(|e| matches!(e, BotEvent::PathStop { .. })));
        assert_eq!(
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::NoPath))),
            "facade goal must be cleared with the failure recorded"
        );
    }

    /// The other half of the wedge: a goal that *became* satisfied while
    /// its search lived and died inside the tick gap resolves
    /// `goal_reached`, never `noPath` — upstream's monitor checks `isEnd`
    /// per physicsTick regardless of path state.
    #[test]
    fn bridge_wedged_satisfied_goal_reports_reached() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        facade_mut(&client, |f| {
            // a YGoal the bot is already standing on
            f.goal = Some(Goal::Y { y: 64 });
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::GoalReached { .. })),
            "expected goal_reached, got {out:?}"
        );
        assert!(!out.iter().any(|e| matches!(e, BotEvent::PathUpdate { .. })));
        assert!(facade(&client, |f| f.goal.is_none()).unwrap_or(false));
    }

    /// Upstream's `thinkTimeout` is a cumulative promise budget: a goal
    /// that keeps producing *partial* paths without ever completing must
    /// reject `Timeout` once its searches total the budget — azalea
    /// would otherwise repath the unreachable frontier forever. The ECS
    /// goal and the partial path keep walking (upstream's
    /// `monitorMovement` survives a timeout reject); only the facade's
    /// claim on the promise ends.
    #[test]
    fn bridge_cumulative_search_budget_reports_timeout() {
        use std::collections::VecDeque;
        fn noop(_: moves::ExecuteCtx) {}
        fn unreached(_: moves::IsReachedCtx) -> bool {
            false
        }
        let partial_path = |client: &azalea::Client, target| {
            client
                .ecs
                .write()
                .entity_mut(client.entity)
                .insert(ExecutingPath {
                    path: VecDeque::from([pathfinder::astar::Edge {
                        movement: pathfinder::astar::Movement {
                            target,
                            data: moves::MoveData {
                                execute: &noop,
                                is_reached: &unreached,
                            },
                        },
                        cost: 1.,
                    }]),
                    queued_path: None,
                    last_reached_node: BlockPos::new(0, 64, 0),
                    ticks_since_last_node_reached: 0,
                    is_path_partial: true,
                });
        };

        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        // `follow_at` spawns an ECS entity — build it before the
        // facade write, not inside the `ecs.write()` guard
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.goal_gen = 1;
            f.set_gen = 1;
            f.think_timeout = Duration::from_millis(10);
        });
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // repath cycles: each observed search contributes a few ms of
        // spent budget and installs another partial — the goal never
        // completes (saw_success stays false)
        let mut timed_out = false;
        for i in 0..10 {
            {
                let mut ecs = client.ecs.write();
                let mut entity = ecs.get_entity_mut(client.entity).unwrap();
                entity.remove::<ExecutingPath>();
                let mut pf = entity.get_mut::<pathfinder::Pathfinder>().unwrap();
                pf.is_calculating = true;
            }
            assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
            std::thread::sleep(Duration::from_millis(3));
            {
                let mut ecs = client.ecs.write();
                let mut entity = ecs.get_entity_mut(client.entity).unwrap();
                entity
                    .get_mut::<pathfinder::Pathfinder>()
                    .unwrap()
                    .is_calculating = false;
            }
            partial_path(&client, BlockPos::new(i, 64, 0));
            let out = bridge_events(&client, &mut tracker, &Event::Tick);
            if out.iter().any(|e| {
                matches!(
                    e,
                    BotEvent::PathUpdate {
                        status: PathStatus::Timeout,
                        ..
                    }
                )
            }) {
                timed_out = true;
                break;
            }
        }
        assert!(timed_out, "cumulative search spend never rejected");
        assert_eq!(
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::Timeout))),
            "facade goal must be cleared with the timeout recorded"
        );
        // the partial path is still installed — the promise rejected,
        // but upstream keeps walking the best path after a 'timeout'
        assert!(client.is_executing_path());
    }

    /// I-2: `stop()` during an in-flight search must NOT produce a
    /// `path_update` — upstream's aborted `astarContext` is nulled in
    /// `resetPath` and reports nothing. Previously the aborted calc
    /// fell into the `noPath` arm deterministically.
    #[test]
    fn bridge_aborted_calc_emits_no_update() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        // `follow_at` spawns an ECS entity — build it before the
        // facade write, not inside the `ecs.write()` guard
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // what stop() does: clear the ECS goal/calc, then the facade goal
        stop_pathfinding_patched(&client, true);
        facade_mut(&client, |f| f.goal = None);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            !out.iter().any(|e| matches!(e, BotEvent::PathUpdate { .. })),
            "an aborted calc must not emit path_update, got {out:?}"
        );
    }

    /// `newGoal !== goal` — a `set_goal` re-issuing a structurally
    /// identical goal is not a replacement: `set_gen` must not move, so
    /// a pending `goto` survives it (upstream's identity check wouldn't
    /// reject either).
    #[tokio::test]
    async fn goto_survives_identical_goal_reissue() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();
        let pf2 = bot.pathfinder();

        // `follow_at` spawns a fresh entity per call, so the goal must
        // be built off one shared target — Follow identity is the ECS id
        let target_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(500., 64., 0.)))
            .id();
        let mk = |client: &azalea::Client| Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 1.,
        };
        let mut goto = std::pin::pin!(pf.goto(mk(&client)));
        await_install(&mut goto, &client).await;
        let my_gen = facade(&client, |f| f.set_gen);

        pf2.set_goal(mk(&client), false).unwrap();
        assert_eq!(
            facade(&client, |f| f.set_gen),
            my_gen,
            "an identical re-issue bumped set_gen"
        );

        pf2.stop();
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
            .await
            .expect("goto hung")
            .expect_err("the identical re-issue resolved the goto");
        assert!(
            matches!(err, Error::Path(PathError::PathStopped)),
            "expected PathStopped (no GoalChanged), got {err:?}"
        );
    }

    /// The `newGoal !== goal` check's value type analog — same variant,
    /// same parameters. `Follow` compares the target's ECS id; a
    /// different entity is a different goal.
    #[test]
    fn same_goal_identity() {
        let a = Goal::Near {
            pos: Vec3::new(1., 2., 3.),
            radius: 4.,
        };
        assert!(same_goal(&a, &a.clone()));
        assert!(!same_goal(
            &a,
            &Goal::Near {
                pos: Vec3::new(1., 2., 3.),
                radius: 5.
            }
        ));
        assert!(!same_goal(&a, &Goal::Block(BlockPos::new(1, 2, 3))));
        assert!(!same_goal(
            &Goal::LookAtBlock(BlockPos::new(1, 2, 3)),
            &Goal::BreakBlock(BlockPos::new(1, 2, 3))
        ));

        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let e1 = client.ecs.write().spawn(()).id();
        let e2 = client.ecs.write().spawn(()).id();
        let f1 = Goal::Follow {
            entity: client.entity_ref_for(e1),
            range: 2.,
        };
        assert!(same_goal(
            &f1,
            &Goal::Follow {
                entity: client.entity_ref_for(e1),
                range: 2.
            }
        ));
        assert!(!same_goal(
            &f1,
            &Goal::Follow {
                entity: client.entity_ref_for(e2),
                range: 2.
            }
        ));
    }

    /// M-6: `Goal::Follow`'s `is_end` must answer with the same
    /// block-center `RadiusGoal` check the installed goal runs — a raw
    /// Vec3 distance disagrees by ±0.5 per axis and would report a
    /// reached goal as still pending at the boundary.
    #[test]
    fn follow_is_end_uses_block_center() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        // entity (0.4, 64.5, 0.4) vs block (0,64,0) center (0.5,64.5,
        // 0.5): ≈0.14 — in range at r=0.5. The old Vec3 check against
        // the bot's origin would measure ≈0.75 and disagree.
        let target_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(0.4, 64.5, 0.4)))
            .id();
        let goal = Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 0.5,
        };
        assert!(goal.is_end(&client, client.position()));

        // past the center radius → out of range
        let far_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(1.6, 64.5, 0.4)))
            .id();
        let far = Goal::Follow {
            entity: client.entity_ref_for(far_entity),
            range: 0.5,
        };
        assert!(!far.is_end(&client, client.position()));

        // a despawned target is never satisfied — upstream's null-target
        // follow can't be met either
        let mut ecs = client.ecs.write();
        ecs.entity_mut(far_entity).despawn();
        drop(ecs);
        assert!(!far.is_end(&client, client.position()));
    }

    /// `results.status` mapping for a finished search — the contract
    /// `getPathTo`/`getPathFromTo` share with the bridge's failure
    /// classification.
    #[test]
    fn search_result_status_mapping() {
        let budget = Duration::from_secs(5);
        // complete path
        assert_eq!(
            classify_search_result(false, false, Duration::ZERO, budget),
            PathStatus::Success
        );
        // non-empty partial
        assert_eq!(
            classify_search_result(true, false, Duration::ZERO, budget),
            PathStatus::Partial
        );
        // empty result inside budget = exhausted space
        assert_eq!(
            classify_search_result(true, true, Duration::from_secs(1), budget),
            PathStatus::NoPath
        );
        // empty result at/over budget = timed out
        assert_eq!(
            classify_search_result(true, true, budget, budget),
            PathStatus::Timeout
        );
    }

    /// `think_timeout`/`is_mining`/`is_building` accessors — the trivial
    /// facade-backed surface.
    #[test]
    fn pathfinder_accessors() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client);
        let pf = bot.pathfinder();
        assert_eq!(pf.think_timeout(), DEFAULT_THINK_TIMEOUT);
        pf.set_think_timeout(Duration::from_millis(1234));
        assert_eq!(pf.think_timeout(), Duration::from_millis(1234));
        // no Mining component → not mining; azalea never places → never building
        assert!(!pf.is_mining());
        assert!(!pf.is_building());
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
