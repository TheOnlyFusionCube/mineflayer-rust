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
//! `Movements`, watcher cancellation) lives in the [`PathfinderFacade`] ECS
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
};

use azalea::{
    BlockPos, EntityRef, Event, Vec3, ecs as bevy_ecs,
    ecs::prelude::Component,
    entity::Position,
    pathfinder::{
        self, ExecutingPath, PathfinderClientExt, PathfinderOpts,
        custom_state::CustomPathfinderState,
        goals::{self, Goal as AzaleaGoal},
        moves, player_pos_to_block_pos,
        positions::RelBlockPos,
    },
    registry::builtin::BlockKind,
    world::ChunkStorage,
};
use tokio::sync::broadcast;

use crate::{Bot, BotEvent, Error, Result};

/// Cost added to an edge that enters a `blocks_to_avoid` block — the same
/// 100 upstream's `Movements` uses for an unsafe step.
const AVOID_STEP_COST: f32 = 100.0;

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
    /// `goals.GoalY(y)` → [`goals::YGoal`].
    Y {
        /// Target y coordinate.
        y: i32,
    },
    /// `goals.GoalInvert(goal)` — succeeds exactly where the inner goal
    /// fails. Negating the heuristic misleads A* the way azalea's
    /// deprecated `InverseGoal` did (`diverges`).
    Invert(Box<Goal>),
    /// `goals.GoalFollow(entity, range)` — a watcher task re-issues a
    /// radius goal whenever the entity's block position changes
    /// (`diverges`: re-paths on whole-block moves rather than upstream's
    /// continuous moving goal).
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
    /// The azalea goal for pathing, or `None` for [`Goal::Follow`] (and
    /// any composite containing one) — follows are driven by a watcher,
    /// not a static goal. `chunks` is the bot's live chunk storage, needed
    /// only by [`Goal::GetToBlock`]'s line-of-sight check.
    fn to_azalea(&self, chunks: &ChunkStorage) -> Option<Arc<dyn AzaleaGoal>> {
        match self {
            Goal::Near { pos, radius } => Some(Arc::new(goals::RadiusGoal::new(*pos, *radius))),
            Goal::Block(pos) => Some(Arc::new(goals::BlockPosGoal(*pos))),
            Goal::GetToBlock(pos) => Some(Arc::new(goals::ReachBlockPosGoal::new(
                *pos,
                chunks.clone(),
            ))),
            Goal::XZ { x, z } => Some(Arc::new(goals::XZGoal { x: *x, z: *z })),
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
            Goal::Follow { .. } => None,
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
    fn to_opts(&self) -> PathfinderOpts {
        PathfinderOpts::new()
            .successors_fn(movements_move)
            .allow_mining(self.can_dig)
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
#[derive(Component, Default)]
pub(crate) struct PathfinderFacade {
    /// The last `set_goal` argument. `None` once the goal is reached or
    /// abandoned (cleared by the bridge diff, `stop`, or a watcher exit).
    pub goal: Option<Goal>,
    /// Bumped every `set_goal` — lets the bridge tell "goal was replaced"
    /// from "goal was abandoned" when it would otherwise clobber a new
    /// goal with a stale clear.
    pub goal_gen: u64,
    /// The `setGoal` `dynamic` argument — suppresses `goal_reached` and
    /// re-paths whenever the goal stops holding (upstream semantics).
    pub dynamic: bool,
    /// `bot.pathfinder.movements`.
    pub movements: Movements,
    /// Set to cancel the running follow/dynamic watcher.
    watcher_cancel: Option<Arc<AtomicBool>>,
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
    /// reached; errors when the path is stopped or the connection ends
    /// first. (`dynamic` goals are set via [`Pathfinder::set_goal`].)
    ///
    /// Events are the fast path; the tick poll is the source of truth —
    /// the event stream floods with packets on a busy world, and a lagged
    /// receiver could skip the one `goal_reached` this future waits on.
    pub async fn goto(&self, goal: Goal) -> Result<()> {
        // subscribe before set_goal so an already-satisfied goal can't
        // emit `GoalReached` before we listen
        let mut rx = self.bot.events();
        let mut ticks = self.client().get_tick_broadcaster();
        let client = self.client().clone();
        let probe = goal.clone();
        self.set_goal(goal, false)?;
        loop {
            tokio::select! {
                biased;
                r = rx.recv() => {
                    match r {
                        Ok(BotEvent::GoalReached) => return Ok(()),
                        Ok(BotEvent::PathStop) => {
                            return Err(Error::Operation("path stopped".to_string()));
                        }
                        Ok(BotEvent::End(_)) => return Err(Error::NotConnected),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(Error::NotConnected);
                        }
                    }
                }
                t = ticks.recv() => {
                    if matches!(t, Err(broadcast::error::RecvError::Closed)) {
                        return Err(Error::NotConnected);
                    }
                    // a Lagged tick still means time passed — poll anyway.
                    // the facade goal is cleared on reach/stop, so resolve
                    // from state, lagged events or not
                    if facade(&client, |f| f.goal.is_some()) == Some(false) {
                        return if probe.is_end(&client, client.position()) {
                            Ok(())
                        } else {
                            Err(Error::Operation("path stopped".to_string()))
                        };
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
    /// stops holding instead of completing (implicit for
    /// [`Goal::Follow`], which is dynamic by construction).
    pub fn set_goal(&self, goal: Goal, dynamic: bool) -> Result<()> {
        let client = self.client().clone();
        self.bot.emit_event(BotEvent::GoalUpdated);
        if client.is_executing_path() {
            self.bot.emit_event(BotEvent::PathReset {
                reason: "goal_updated",
            });
        }
        let movements = self.movements();
        sync_constraints(&client, &movements);
        // Already satisfied — azalea would drop the goto without ever
        // storing the goal, so mirror upstream's monitor: emit
        // goal_reached now, and just stop the in-flight path (upstream's
        // resetPath also cleared it).
        if !dynamic && goal.is_end(&client, client.position()) {
            client.stop_pathfinding();
            facade_mut(&client, |f| {
                cancel_watcher(f);
                f.goal = None;
                f.goal_gen += 1;
            });
            self.bot.emit_event(BotEvent::GoalReached);
            return Ok(());
        }
        facade_mut(&client, |f| {
            cancel_watcher(f);
            f.goal = Some(goal.clone());
            f.dynamic = dynamic;
            f.goal_gen += 1;
        });
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
                    movements,
                    cancel,
                    self.bot.clone(),
                );
            }
            _ => {
                let chunks = client.world().read().chunks.clone();
                let Some(azalea_goal) = goal.to_azalea(&chunks) else {
                    return Err(Error::Operation(
                        "Goal::Follow can't be nested inside a composite goal".to_string(),
                    ));
                };
                client.start_goto_with_opts(SharedGoal(azalea_goal.clone()), movements.to_opts());
                if dynamic {
                    let cancel = Arc::new(AtomicBool::new(false));
                    facade_mut(&client, |f| {
                        cancel_watcher(f);
                        f.watcher_cancel = Some(cancel.clone());
                    });
                    dynamic_watcher(client.clone(), azalea_goal, movements, cancel);
                }
            }
        }
        Ok(())
    }

    /// `bot.pathfinder.stop()` — stop navigating now (upstream clears the
    /// path immediately, so this is `force_stop_pathfinding`, not the
    /// mid-move-friendly `stop_pathfinding`). Emits `path_stop` here like
    /// upstream's `stop()`; azalea's goal clear then arrives with the
    /// facade goal already `None`, so the bridge doesn't re-report it.
    pub fn stop(&self) {
        let client = self.client();
        // upstream: `if (!stateGoal && path.length === 0) return` — an
        // idle stop emits nothing
        let active = !client.is_goto_target_reached()
            || facade(client, |f| f.goal.is_some()).unwrap_or(false);
        client.force_stop_pathfinding();
        facade_mut(client, |f| {
            cancel_watcher(f);
            f.goal = None;
        });
        // `handle_stop_pathfinding_event` drops an in-flight ComputePath
        // without resetting `is_calculating` — azalea 0.16 never clears
        // the flag in that case, which would hang
        // `is_goto_target_reached`/the `goto` future forever. Write the
        // same values azalea's handler would.
        let _ = client.try_query_self::<&mut pathfinder::Pathfinder, _>(|mut p| {
            p.goal = None;
            p.is_calculating = false;
        });
        if active {
            self.bot.emit_event(BotEvent::PathStop);
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
}

/// Cancel the follow/dynamic watcher if one is running — the flag, not a
/// task handle, so a stale watcher can never clobber a successor's state.
fn cancel_watcher(facade: &mut PathfinderFacade) {
    if let Some(cancel) = facade.watcher_cancel.take() {
        cancel.store(true, atomic::Ordering::Relaxed);
    }
}

/// The [`Goal::Follow`] driver: every tick, re-issue a radius goal on the
/// entity's new block position (azalea merges it into the running path).
/// Ends silently when the facade goal is gone — a non-dynamic follow ends
/// once the bridge reports `goal_reached`, and `stop()`/`set_goal` cancel
/// through the flag. The entity leaving the world mirrors upstream's
/// `isValid()` failure: the driver clears the facade goal, stops
/// pathfinding, and reports `path_stop` itself (the ECS goal may never
/// have been installed, so the diff can't see it).
fn follow_driver(
    client: azalea::Client,
    entity: EntityRef,
    range: f32,
    movements: Movements,
    cancel: Arc<AtomicBool>,
    bot: Bot,
) {
    tokio::spawn(async move {
        let mut ticks = client.get_tick_broadcaster();
        let mut last_target: Option<BlockPos> = None;
        let mut despawned = false;
        while ticks.recv().await.is_ok() && !cancel.load(atomic::Ordering::Relaxed) {
            if facade(&client, |f| f.goal.is_some()) != Some(true) {
                // reached (non-dynamic), stopped, or replaced — whoever
                // cleared it already reported the outcome
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
                movements.to_opts(),
            );
        }
        if cancel.load(atomic::Ordering::Relaxed) || !despawned {
            return;
        }
        // the entity despawned — the follow goal is no longer valid
        facade_mut(&client, |f| f.goal = None);
        client.stop_pathfinding();
        bot.emit_event(BotEvent::PathStop);
    });
}

/// The `dynamic`-goal watcher: once azalea clears a reached goal, keep
/// re-issuing it whenever the goal's success check stops holding —
/// upstream's "dynamic goals never complete" semantics, which azalea's
/// goal-clears-on-success can't express on its own.
fn dynamic_watcher(
    client: azalea::Client,
    goal: Arc<dyn AzaleaGoal>,
    movements: Movements,
    cancel: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut ticks = client.get_tick_broadcaster();
        while ticks.recv().await.is_ok() && !cancel.load(atomic::Ordering::Relaxed) {
            if !client.is_goto_target_reached() {
                continue;
            }
            // while the goal still holds, idle at it — upstream keeps
            // stateGoal set and satisfied
            if goal.success(player_pos_to_block_pos(client.position())) {
                continue;
            }
            if facade(&client, |f| f.goal.is_some()) != Some(true) {
                break;
            }
            client.start_goto_with_opts(SharedGoal(goal.clone()), movements.to_opts());
        }
    });
}

/// Per-tick diff state for the path events — see [`bridge_events`].
#[derive(Default)]
pub(crate) struct PathTracker {
    last: Option<PathSnapshot>,
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
    /// `PathfinderFacade.goal`/`goal_gen`/`dynamic`.
    facade_goal: Option<Goal>,
    facade_gen: u64,
    dynamic: bool,
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

    // `path_update` — a path got installed (possibly merged mid-run by a
    // repath) or a calculation finished.
    if (!prev.executing && now.executing) || (prev.is_calculating && !now.is_calculating) {
        out.push(BotEvent::PathUpdate {
            path: now.path.clone(),
            is_partial: now.is_partial,
        });
    }

    // `goal_reached` / `path_stop` — azalea cleared its goal.
    // `now.facade_goal.is_some()` gates the whole thing: `stop()` and the
    // follow driver clear the facade goal when they report, so a clear
    // arriving with it already `None` was already announced. Satisfaction
    // is checked against the *current* facade goal (`is_end`), which also
    // covers a replaced goal that instant-satisfied within the same tick.
    if prev.ecs_goal.is_some()
        && now.ecs_goal.is_none()
        && let Some(cur_goal) = now.facade_goal.as_ref()
    {
        let satisfied = now.pos.is_some_and(|pos| cur_goal.is_end(client, pos));
        if satisfied && !now.dynamic {
            facade_mut(client, |f| f.goal = None);
            out.push(BotEvent::GoalReached);
        } else if !satisfied && now.facade_gen == prev.facade_gen {
            // the same goal was abandoned without reaching (world change,
            // azalea-internal stop). A gen bump means the cleared goal
            // belonged to an older set_goal — its death was already
            // announced by that call's path_reset, so say nothing.
            facade_mut(client, |f| f.goal = None);
            out.push(BotEvent::PathStop);
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
}
