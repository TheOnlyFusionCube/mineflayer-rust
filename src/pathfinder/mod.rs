//! The mineflayer-pathfinder plugin surface: `bot.pathfinder()` with
//! `goto`/`setGoal`/`stop`, the `Goal*` family mapped onto the engine goals, a
//! `Movements` config, and the `path_update`/`goal_reached`/`goal_updated`/
//! `path_stop`/`path_reset` events.
//!
//! The engine's pathfinder is goal-driven: a `GotoEvent` stores
//! `Arc<dyn Goal>` + [`PathfinderOpts`] on the `Pathfinder` component and its
//! systems repath on obstruction, timeout, and partial-path completion while
//! the goal stands — the same recovery loop upstream drives by re-running
//! A* after `path_reset`, so mid-path fixes come free. Facade bookkeeping
//! The engine has no slot for (the [`Goal`] enum the user set, `dynamic`,
//! `Movements`, watcher cancellation) lives in the `PathfinderFacade` ECS
//! component on the client entity.
//!
//! Path events come from the engine's `Event::Pathfinder` stream (path
//! found with path/is_partial/elapsed, goal reached, stopped) — see
//! `tracking`. One piece of bookkeeping needs care beyond translation:
//!
//! - upstream's `thinkTimeout` bounds one A* end-to-end, but the engine
//!   splits the search into a fresh `max_timeout` per repath, so the
//!   budget is tracked cumulatively per goal issue — partial progress
//!   can't keep a `goto` pending forever (upstream's `timeout`). The
//!   reject is promise-level like upstream's: the facade goal is the
//!   surviving `stateGoal` and stays installed (watchers and partial
//!   paths keep running); only `last_failure` records the outcome so a
//!   pending `goto` can settle.
//!
//! - `path_update` — a path was installed, a repath result merged
//!   mid-run (the executing path grows — consumption only shrinks), or
//!   a calculation finished (the engine clears `is_calculating` for every
//!   result, failures included, matching upstream's per-result
//!   `path_update`).
//! - `goal_reached` — the engine cleared the goal while its success check
//!   holds (suppressed for `dynamic` goals, like upstream).
//! - `path_stop` — the goal was abandoned without reaching (emitted by
//!   `stop()`, by a follow-goal going invalid, or here for the engine-internal
//!   clears like world changes; deduped via the facade goal).
//! - `goal_updated` / `path_reset` — emitted synchronously by
//!   `set_goal`/`set_movements` at upstream's call sites.
//!
//! The engine's own repaths (patch/timeout/partial-end) surface only as
//! `path_update`s — upstream's per-reason `path_reset`s (`stuck`,
//! `block_updated`, `chunk_loaded`, …) aren't distinguishable from outside
//! the runner and are a documented `diverges`.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{self, AtomicBool},
    },
    time::Duration,
};

use mineflayer_engine::{
    BlockPos, EntityRef, Event, PathEvent, Vec3, ecs as bevy_ecs,
    ecs::prelude::Component,
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, Position, inventory::Inventory,
        mining::get_mine_progress,
    },
    pathfinder::{
        self, CalculatePathCtx, PathfinderClientExt, PathfinderOpts, calculate_path,
        custom_state::CustomPathfinderState,
        goals::{self, Goal as EngineGoal},
        mining::MiningCache,
        moves, player_pos_to_block_pos,
        positions::RelBlockPos,
    },
    registry::builtin::BlockKind,
    world::ChunkStorage,
};
use tokio::sync::{broadcast, watch};

mod goal;
mod movements;
mod tracking;

pub use goal::Goal;
use goal::*;
pub use movements::Movements;
use movements::*;
pub(crate) use tracking::{PathTracker, bridge_events};

use crate::bot::{TickStream, with_component, with_component_mut};
use crate::{Bot, BotEvent, Error, Result, windows::Item, world::Block};

/// Cost added to an edge that enters a `blocks_to_avoid` block — the same
/// 100 upstream's `Movements` uses for an unsafe step.
const AVOID_STEP_COST: f32 = 100.0;

/// `bot.pathfinder.thinkTimeout` upstream (5000 ms) — the per-search time
/// budget; the engine's `PathfinderOpts::max_timeout` plays the same role,
/// and the bridge applies it cumulatively across repaths (see
/// [`bridge_events`]) since upstream's sliced A* shares one budget.
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
/// `results` object reduced to its observed fields (the engine computes the
/// whole search in one pass, so there are no intermediate yields).
#[derive(Clone, Debug)]
pub struct PathResult {
    /// `results.status`.
    pub status: PathStatus,
    /// `results.path` — the block positions to walk (empty when the
    /// search found nothing).
    pub path: Vec<BlockPos>,
}

/// Facade state the engine has no slot for: the [`Goal`] the user set (the engine
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

/// Publish the `Movements` constraints into `CustomPathfinderState` so
/// [`movements_move`] sees them during the next search.
///
/// lock rule: the state lock is taken with no ECS guard held — the value
/// is copied out of the ECS first (the search thread takes this state
/// lock while holding nothing, so the reverse order would cross it).
fn sync_constraints(client: &mineflayer_engine::Client, movements: &Movements) {
    // lock rule (bot/mod.rs): the state lock is taken after the ECS guard
    // is dropped — never the reverse
    let state = {
        let mut ecs = client.ecs.write();
        let Ok(mut entity) = ecs.get_entity_mut(client.entity) else {
            return;
        };
        if entity.get::<CustomPathfinderState>().is_none() {
            entity.insert(CustomPathfinderState::default());
        }
        let Some(state) = entity.get::<CustomPathfinderState>().map(|s| s.0.clone()) else {
            return;
        };
        state
    };
    state.write().insert(MovementConstraints::from(movements));
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
    fn client(&self) -> &mineflayer_engine::Client {
        &self.bot.client
    }

    /// `bot.pathfinder.goto(goal)` — set the goal and resolve when it's
    /// reached; errors on `path_stop` (PathStopped), a failed search
    /// (NoPath/Timeout, matching upstream's `results.status` handling in
    /// `pf-goto.js`), a goal replacement (GoalChanged), or disconnect.
    /// (`dynamic` goals are set via [`Pathfinder::set_goal`].)
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// let goal = Goal::Near {
    ///     pos: Vec3::new(100.0, 64.0, -20.0),
    ///     radius: 2.0,
    /// };
    /// bot.pathfinder().goto(goal).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Events are the fast path; the tick poll is the source of truth —
    /// the event stream floods with packets on a busy world, and a lagged
    /// receiver could skip the one event this future waits on. The
    /// `set_gen`/`last_failure` facade fields give the poll the same
    /// discrimination the events carry.
    pub async fn goto(&self, goal: Goal) -> Result<()> {
        // subscribe before set_goal so an already-satisfied goal can't
        // emit `GoalReached` before we listen
        let mut rx = self.bot.subscribe();
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
                            // when the bridge recorded OUR failure —
                            // `set_gen` still ours and `last_failure`
                            // carrying the same outcome. The facade goal
                            // may still be installed (a `Timeout` leaves
                            // upstream's `stateGoal` running) or already
                            // cleared (`NoPath`); anything else is stale
                            // (a previous goal's update, or an aborted
                            // calc's `noPath` report) — skip it.
                            if matches!(status, PathStatus::NoPath | PathStatus::Timeout)
                                && let Some(err) = with_component::<PathfinderFacade, _>(&client, |f| {
                                    (f.set_gen == my_gen)
                                        .then_some(f.last_failure)
                                        .flatten()
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
                            if with_component::<PathfinderFacade, _>(&client, |f| f.set_gen) != Some(my_gen) {
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
                    // resolve from state, lagged events or not: the
                    // facade goal is cleared on reach/stop/`NoPath`, but a
                    // `Timeout` leaves the goal installed (upstream's
                    // surviving `stateGoal`) with only `last_failure`
                    // recording the outcome
                    match with_component::<PathfinderFacade, _>(&client, |f| {
                        (f.set_gen, f.goal.is_some(), f.last_failure)
                    }) {
                        // a `set_goal` after ours — the `goal_updated`
                        // event may never have reached us (Lagged)
                        Some((set_gen, _, _)) if set_gen != my_gen => {
                            return Err(PathError::GoalChanged.into());
                        }
                        // a recorded failure rejects whether the goal is
                        // still installed (Timeout) or cleared (NoPath)
                        Some((_, _, Some(failed))) => {
                            return Err(failed.into());
                        }
                        Some((_, false, _)) => {
                            return if probe.is_end(&client, client.position()) {
                                Ok(())
                            } else {
                                Err(PathError::PathStopped.into())
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
        // Already satisfied — the engine would drop the goto without ever
        // storing the goal, so mirror upstream's monitor: `goal_updated`
        // then `goal_reached`, and stop the in-flight path (upstream's
        // resetPath also cleared it).
        let satisfied = !dynamic && goal.is_end(&client, client.position());
        // the facade write lands BEFORE the ECS patch — same interleave
        // argument as `stop()` (N-3): an ownerless installed goal is
        // silent, an ownerless clear edge is a phantom `NoPath`
        let set_gen = with_component_mut::<PathfinderFacade, _>(&client, |f| {
            cancel_watcher(f);
            // `newGoal !== goal` — upstream's identity check wouldn't
            // reject a goto when the identical goal is re-issued, so an
            // equal goal doesn't bump `set_gen` (the re-path below still
            // happens; only the replacement flag is deduped). `dynamic`
            // isn't part of the identity — upstream compares only the
            // goal object (N-5).
            let same = f.goal.as_ref().is_some_and(|g| same_goal(g, &goal));
            f.goal = (!satisfied).then(|| goal.clone());
            f.dynamic = dynamic;
            f.goal_gen += 1;
            if !same {
                f.set_gen += 1;
            }
            f.last_failure = None;
            f.set_gen
        })
        .ok_or_else(|| {
            Error::InvalidState("pathfinder facade is missing — client entity is gone".into())
        })?;
        if satisfied {
            client.stop_pathfinding();
        }
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
            with_component_mut::<PathfinderFacade, _>(&client, |f| f.goal = None);
        }
        result.map(|_| set_gen)
    }

    /// Issue a goal to the engine (or its watcher task for
    /// [`Goal::Follow`]/`dynamic`) after `set_goal` has done the facade
    /// bookkeeping.
    fn issue(&self, goal: Goal, dynamic: bool, movements: Movements) -> Result<()> {
        let client = self.client().clone();
        // Watchers re-check `goal_gen` against this before every
        // re-issue — a `set_goal` since their spawn bumps it, so they
        // can never write a stale `GotoEvent` over a replacement goal
        // (the engine's `goto_listener` keeps the last event, and a stale
        // goal would later emit a spurious `path_stop`).
        let (issue_gen, think_timeout) =
            with_component::<PathfinderFacade, _>(&client, |f| (f.goal_gen, f.think_timeout))
                .unwrap_or_default();
        let opts = movements.to_opts().max_timeout(think_timeout);
        match goal {
            Goal::Follow { entity, range } => {
                let cancel = Arc::new(AtomicBool::new(false));
                // set_movements re-issues through here too — always retire
                // the previous watcher before starting a new one
                with_component_mut::<PathfinderFacade, _>(&client, |f| {
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
                    self.bot.driver_handle(),
                    issue_gen,
                );
            }
            _ => {
                let chunks = client.world().read().chunks.clone();
                let Some(engine_goal) = goal.to_engine(&chunks) else {
                    return Err(Error::NotFound(
                        "Goal::Follow's entity isn't loaded".to_string(),
                    ));
                };
                client.start_goto_with_opts(SharedGoal(engine_goal.clone()), opts);
                if dynamic {
                    let cancel = Arc::new(AtomicBool::new(false));
                    with_component_mut::<PathfinderFacade, _>(&client, |f| {
                        cancel_watcher(f);
                        f.watcher_cancel = Some(cancel.clone());
                    });
                    dynamic_watcher(
                        client.clone(),
                        engine_goal,
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
    /// upstream's `stop()`; the engine's goal clear then arrives with the
    /// facade goal already `None`, so the bridge doesn't re-report it.
    pub fn stop(&self) {
        let client = self.client();
        // upstream: `if (!stateGoal && path.length === 0) return` — an
        // idle stop emits nothing
        let active = !client.is_goto_target_reached()
            || with_component::<PathfinderFacade, _>(client, |f| f.goal.is_some()).unwrap_or(false);
        // the facade write lands BEFORE the engine's stop handler runs:
        // the opposite order lets a bridge tick see `calc_finished &&
        // goal_cleared` with the facade goal still set and report a
        // phantom `NoPath` a pending `goto` would trust over `PathStopped`
        // (N-3). This order shows an ownerless installed goal — every arm
        // gates on the facade goal, so it's silent.
        let stopped = with_component_mut::<PathfinderFacade, _>(client, |f| {
            cancel_watcher(f);
            f.last_failure = None;
            f.goal.take()
        })
        .flatten();
        client.force_stop_pathfinding();
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
        with_component::<PathfinderFacade, _>(self.client(), |f| f.goal.clone()).flatten()
    }

    /// `bot.pathfinder.movements` — the current `Movements` config
    /// (upstream defaults until `set_movements` runs).
    pub fn movements(&self) -> Movements {
        with_component::<PathfinderFacade, _>(self.client(), |f| f.movements.clone())
            .unwrap_or_default()
    }

    /// `bot.pathfinder.setMovements(movements)` — replaces the config and,
    /// like upstream, abandons the in-progress path (`path_reset` with
    /// `movements_updated`) and re-paths under the new movements.
    pub fn set_movements(&self, movements: Movements) {
        let client = self.client().clone();
        sync_constraints(&client, &movements);
        let was_executing = client.is_executing_path();
        let goal_and_dynamic = with_component_mut::<PathfinderFacade, _>(&client, |f| {
            f.movements = movements.clone();
            // re-issuing is a new goal generation — watchers still
            // holding the previous generation stop instead of
            // overwriting the fresh goto with the old movements.
            // `last_failure` is per-generation too: a stale `Timeout`
            // must not reject the re-issued goal's pending `goto`
            f.goal_gen += 1;
            f.last_failure = None;
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
    /// The engine as `PathfinderOpts::max_timeout`; also the NoPath-vs-Timeout
    /// threshold the bridge classifies a failed search against.
    pub fn think_timeout(&self) -> Duration {
        with_component::<PathfinderFacade, _>(self.client(), |f| f.think_timeout)
            .unwrap_or(DEFAULT_THINK_TIMEOUT)
    }

    /// `bot.pathfinder.thinkTimeout = ms` — affects goals issued from
    /// this point on (upstream mutates the live search context; ours is
    /// read at issue time).
    pub fn set_think_timeout(&self, timeout: Duration) {
        with_component_mut::<PathfinderFacade, _>(self.client(), |f| f.think_timeout = timeout);
    }

    /// `bot.pathfinder.isMining()` — whether the bot is currently mining
    /// a block (the engine's `Mining` component). `diverges`: upstream's flag
    /// is the *pathfinder's own* `digging` state; the engine's component
    /// counts any in-progress client mining — a manual `bot.dig` in
    /// flight reports true here where upstream's would not.
    pub fn is_mining(&self) -> bool {
        self.client().is_mining()
    }

    /// `bot.pathfinder.isBuilding()` — always `false`: the engine's
    /// successors never produce scaffolding placements, so the bot never
    /// builds while pathing (`diverges` — upstream can return true).
    pub fn is_building(&self) -> bool {
        false
    }

    /// `bot.pathfinder.bestHarvestTool(block)` — the inventory item that
    /// digs `block` fastest, via the engine's `get_mine_progress` (accounts
    /// for held-item rules, water on the eyes, haste/fatigue). `None`
    /// when the inventory is empty, the entity isn't loaded, or every
    /// tool's dig time is infinite (unbreakable — upstream's
    /// `digTime: Infinity` keeps `bestTool` at `null`, and zero mine
    /// progress is the same signal here). Scans `player_slots_range` —
    /// main inventory + hotbar, the range upstream's
    /// `inventory.items()` covers; crafting/armor/offhand slots are out.
    pub fn best_harvest_tool(&self, block: &Block) -> Option<Item> {
        let client = self.client();
        let block_trait = Box::<dyn mineflayer_engine::block::BlockTrait>::from(block.state);
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
    /// keeps an `astarContext` for follow-up computes; the engine's search
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
    /// have no the engine knob and are not exposed.
    pub fn get_path_from_to(
        &self,
        movements: &Movements,
        start: BlockPos,
        goal: &Goal,
        timeout: Option<Duration>,
    ) -> Result<PathResult> {
        let client = self.client();
        let chunks = client.world().read().chunks.clone();
        let Some(engine_goal) = goal.to_engine(&chunks) else {
            return Err(Error::NotFound(
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
        // a fresh goto counter: the preview is never superseded, and it
        // can't disturb a real goto's id
        let event = calculate_path(CalculatePathCtx {
            entity: client.entity,
            start,
            goal: engine_goal,
            world_lock: client.world(),
            goto_id_atomic: Arc::new(atomic::AtomicUsize::new(0)),
            mining_cache: MiningCache::new(inventory_menu),
            custom_state,
            opts,
        })
        .ok_or_else(|| Error::Cancelled("path calculation superseded mid-search".to_string()))?;
        let path: Vec<BlockPos> = event
            .path
            .map(|edges| edges.iter().map(|e| e.movement.target).collect())
            .unwrap_or_default();
        let status =
            classify_search_result(event.is_partial, path.is_empty(), event.elapsed, timeout);
        Ok(PathResult { status, path })
    }
}

/// Upstream's `results.status` for a finished engine search — a
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

/// Stop the engine's pathfinder (`force` = halt mid-move) and patch the
/// component state its 0.16 stop handler drops:
/// The [`Goal::Follow`] driver: every tick, re-issue a radius goal on the
/// entity's new block position (the engine merges it into the running path).
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
    client: mineflayer_engine::Client,
    entity: EntityRef,
    range: f32,
    movements: Movements,
    cancel: Arc<AtomicBool>,
    bot: Bot,
    issue_gen: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = TickStream::new(&client, bot.shutdown.clone());
        let mut last_target: Option<BlockPos> = None;
        // the goal was just issued — a drop observed on the first poll
        // is a fresh one
        let mut was_live = true;
        let mut despawned = false;
        loop {
            if !ticks.next().await {
                return;
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
            if with_component::<PathfinderFacade, _>(&client, |f| {
                f.goal.is_some() && f.goal_gen == issue_gen
            }) != Some(true)
            {
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
            let think_timeout = with_component::<PathfinderFacade, _>(&client, |f| f.think_timeout)
                .unwrap_or(DEFAULT_THINK_TIMEOUT);
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
        let stopped = with_component_mut::<PathfinderFacade, _>(&client, |f| {
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
        client.stop_pathfinding();
        bot.emit_event(BotEvent::PathStop { goal: Some(goal) });
    })
}

/// The `dynamic`-goal watcher: once the engine clears a reached goal, keep
/// re-issuing it whenever the goal's success check stops holding —
/// upstream's "dynamic goals never complete" semantics, which the engine's
/// goal-clears-on-success can't express on its own. `goal_gen` gates the
/// re-issue so a watcher can't write a stale `GotoEvent` over a goal
/// `set_goal`/`set_movements` installed after it spawned. Disconnect
/// retires it through `Bot::shutdown`, like [`follow_driver`].
fn dynamic_watcher(
    client: mineflayer_engine::Client,
    goal: Arc<dyn EngineGoal>,
    movements: Movements,
    cancel: Arc<AtomicBool>,
    shutdown: watch::Receiver<()>,
    issue_gen: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticks = TickStream::new(&client, shutdown);
        // the goal was just issued — a drop observed on the first poll
        // is a fresh one
        let mut was_live = true;
        loop {
            if !ticks.next().await {
                return;
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
            let Some(think_timeout) = with_component::<PathfinderFacade, _>(&client, |f| {
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
/// hasn't run yet (the engine's goal drop lands in Update, the bridge sees
/// it on the next GameTick), and a re-issue inside that window would
/// install an unclaimed goal the bot then walks (M-4).
fn pathfinder_live(client: &mineflayer_engine::Client) -> bool {
    client
        .try_query_self::<&pathfinder::Pathfinder, _>(|p| p.goal.is_some() || p.is_calculating)
        .unwrap_or(false)
        || client.is_executing_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client whose ECS carries `TickBroadcast` (watchers park on it)
    /// plus the pathfinder message resources, so a watcher's
    /// `start_goto_with_opts` lands in `Messages<GotoEvent>` where the
    /// test can count it. No systems run — ticks arrive only via
    /// [`crate::test_util::tick_sender`].
    pub(super) fn watcher_client(
        bundle: impl bevy_ecs::bundle::Bundle,
    ) -> mineflayer_engine::Client {
        let mut app = mineflayer_engine::app::App::new();
        app.add_plugins(mineflayer_engine::tick_broadcast::TickBroadcastPlugin)
            .add_plugins(pathfinder::PathfinderPlugin);
        let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
        let entity = ecs.write().spawn(bundle).id();
        mineflayer_engine::Client::new(entity, ecs)
    }

    /// Count of `GotoEvent`s the watcher has issued so far.
    pub(super) fn goto_messages(client: &mineflayer_engine::Client) -> usize {
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
        let goal: Arc<dyn EngineGoal> = Arc::new(goals::BlockPosGoal(BlockPos::new(100, 64, 100)));
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
        with_component_mut::<PathfinderFacade, _>(&client, |f| {
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
    /// never need a `WorldHolder` (no world lock to build one).
    pub(super) fn follow_at(client: &mineflayer_engine::Client, pos: Vec3) -> Goal {
        let target_entity = client.ecs.write().spawn(Position::new(pos)).id();
        Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 1.,
        }
    }

    /// Drive `goto` until its goal is installed — `set_goal` runs on the
    /// future's first poll, so the watcher arm completes after a round.
    pub(super) async fn await_install(
        goto: &mut std::pin::Pin<&mut impl std::future::Future<Output = Result<()>>>,
        client: &mineflayer_engine::Client,
    ) {
        tokio::select! {
            r = &mut *goto => panic!("goto resolved before its goal was installed: {r:?}"),
            _ = async {
                while with_component::<PathfinderFacade, _>(client, |f| f.goal.is_none()).unwrap_or(true) {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
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
        let mk = |client: &mineflayer_engine::Client| Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 1.,
        };
        let mut goto = std::pin::pin!(pf.goto(mk(&client)));
        await_install(&mut goto, &client).await;
        let my_gen = with_component::<PathfinderFacade, _>(&client, |f| f.set_gen);

        pf2.set_goal(mk(&client), false).unwrap();
        assert_eq!(
            with_component::<PathfinderFacade, _>(&client, |f| f.set_gen),
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
        // no Mining component → not mining; the engine never places → never building
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
        with_component_mut::<PathfinderFacade, _>(&client, |f| {
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

    /// `goto` rejects `Timeout` when the cumulative budget reports —
    /// with the facade goal still installed (upstream's surviving
    /// `stateGoal` keeps repathing; the promise alone settles).
    #[tokio::test]
    async fn goto_rejects_timeout_with_goal_still_running() {
        let client = watcher_client(Position::new(Vec3::new(0., 64., 0.)));
        let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
        let pf = bot.pathfinder();

        let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
        await_install(&mut goto, &client).await;

        // play the bridge's budget outcome: the goal survives with the
        // timeout recorded — the goto's tick arm must still surface it
        with_component_mut::<PathfinderFacade, _>(&client, |f| {
            f.last_failure = Some(PathError::Timeout);
        });
        assert!(
            with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_some()).unwrap_or(false)
        );
        let ticks = crate::test_util::tick_sender(&client);
        while ticks.send(()).is_err() {
            tokio::task::yield_now().await;
        }
        let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
            .await
            .expect("goto hung on a timed-out goal")
            .expect_err("the timed-out goto resolved");
        assert!(
            matches!(err, Error::Path(PathError::Timeout)),
            "expected Timeout, got {err:?}"
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
