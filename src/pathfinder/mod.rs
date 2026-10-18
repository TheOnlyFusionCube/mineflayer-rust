//! The mineflayer-pathfinder plugin surface: `bot.pathfinder()` with
//! `goto`/`setGoal`/`stop`, the `Goal*` family mapped onto the engine goals, a
//! `Movements` config, and the `path_update`/`goal_reached`/`goal_updated`/
//! `path_stop`/`path_reset` events.
//!
//! The engine's pathfinder is goal-driven: a `GotoEvent` stores
//! `Arc<dyn Goal>` + [`PathfinderOpts`] on the `Pathfinder` component and its
//! systems repath on obstruction, timeout, and partial-path completion while
//! the goal stands — the same recovery loop upstream drives by re-running
//! A* after `path_reset`, so mid-path fixes come free. What the engine has
//! no slot for (the [`Goal`] the user set, `dynamic`, `Movements`, the
//! `goto`s waiting on the goal) lives in the `PathfinderFacade` ECS
//! component on the client entity.
//!
//! Each engine goto has a [`GotoId`], and every engine path event names
//! its goto. The facade records the goto its goal runs as now, so the
//! bridge (`tracking`) acts only on the events of that goto: an event of a
//! replaced or stopped goto is stale by construction. Upstream's
//! `thinkTimeout` bounds the searches of one goal end to end; the engine's
//! `total_timeout` sums the partial searches of one goto and reports
//! `Timeout` once, with the goal kept (upstream's surviving `stateGoal`).
//!
//! - `path_update` — a search finished (complete, partial, or failed).
//! - `goal_reached` — the engine reached the goal of the current goto
//!   (suppressed for `dynamic` goals, like upstream).
//! - `path_stop` — the goal was abandoned without reaching (emitted by
//!   `stop()`, by a follow target that despawned, or by the bridge for an
//!   engine-internal stop such as a world change).
//! - `goal_updated` / `path_reset` — emitted synchronously by
//!   `set_goal`/`set_movements` at upstream's call sites.
//!
//! The engine's own repaths (patch/timeout/partial-end) surface only as
//! `path_update`s — upstream's per-reason `path_reset`s (`stuck`,
//! `block_updated`, `chunk_loaded`, …) aren't distinguishable from outside
//! the runner and are a documented `diverges`.

use std::{
    collections::HashSet,
    sync::{Arc, atomic},
    time::Duration,
};

use mineflayer_engine::{
    BlockPos, EntityRef, Event, PathEvent, Vec3, ecs as bevy_ecs,
    ecs::prelude::Component,
    entity::{ActiveEffects, Attributes, FluidOnEyes, Physics, Position, inventory::Inventory},
    pathfinder::{
        CalculatePathCtx, GotoEvent, GotoId, PathfinderClientExt, PathfinderOpts, calculate_path,
        custom_state::CustomPathfinderState,
        goals::{self, Goal as EngineGoal},
        mining::MiningCache,
        moves, player_pos_to_block_pos,
        positions::RelBlockPos,
    },
    registry::builtin::BlockKind,
    world::ChunkStorage,
};
use tokio::sync::oneshot;

mod goal;
mod movements;
mod tracking;

pub use goal::Goal;
pub use movements::Movements;
use movements::*;
pub(crate) use tracking::bridge_events;

use crate::bot::{with_component, with_component_mut};
use crate::{Bot, BotEvent, Error, Result, windows::Item, world::Block};

/// Cost added to an edge that enters a `blocks_to_avoid` block — the same
/// 100 upstream's `Movements` uses for an unsafe step.
const AVOID_STEP_COST: f32 = 100.0;

/// `bot.pathfinder.thinkTimeout` upstream (5000 ms) — the time budget of
/// one goal's searches; the engine gets it as each search's `max_timeout`
/// and as the goto's `total_timeout`.
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
    /// without a complete path.
    Timeout,
}

/// Why `path_reset` fired: upstream's `reason` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathResetReason {
    /// `'goal_updated'` — a `set_goal` replaced the goal mid-path.
    GoalUpdated,
    /// `'movements_updated'` — a `set_movements` changed the movements
    /// mid-path.
    MovementsUpdated,
}

impl PathResetReason {
    /// Upstream's string for the reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GoalUpdated => "goal_updated",
            Self::MovementsUpdated => "movements_updated",
        }
    }
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

/// What a waiting `goto` learns about its goal.
type Waiter = oneshot::Sender<std::result::Result<(), PathError>>;

/// Facade state the engine has no slot for.
#[derive(Component)]
pub(crate) struct PathfinderFacade {
    /// The last `set_goal` argument (upstream's `stateGoal`). `None` once
    /// the goal is reached or abandoned.
    pub goal: Option<Goal>,
    /// The `setGoal` `dynamic` argument — suppresses `goal_reached` and
    /// re-paths whenever the goal stops holding (upstream semantics).
    pub dynamic: bool,
    /// `bot.pathfinder.movements`.
    pub movements: Movements,
    /// `bot.pathfinder.thinkTimeout`.
    pub think_timeout: Duration,
    /// The engine goto that `goal` runs as. A follow step and a
    /// `set_movements` start a new one. `None` while a `dynamic` goal
    /// waits at its target.
    pub goto: Option<GotoId>,
    /// The follow target's block when `goal` last went to the engine.
    follow_block: Option<BlockPos>,
    /// The `goto` calls waiting for `goal`'s outcome.
    waiters: Vec<Waiter>,
}

impl Default for PathfinderFacade {
    fn default() -> Self {
        Self {
            goal: None,
            dynamic: false,
            movements: Movements::default(),
            think_timeout: DEFAULT_THINK_TIMEOUT,
            goto: None,
            follow_block: None,
            waiters: Vec::new(),
        }
    }
}

impl PathfinderFacade {
    /// Give every waiting `goto` its outcome.
    fn settle(&mut self, outcome: std::result::Result<(), PathError>) {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send(outcome);
        }
    }

    /// A goto of `goal` under the facade's movements and budget, recorded
    /// as the goto the goal runs as.
    fn goto_event(
        &mut self,
        entity: bevy_ecs::entity::Entity,
        goal: Arc<dyn EngineGoal>,
    ) -> GotoEvent {
        let opts = self
            .movements
            .to_opts()
            .max_timeout(self.think_timeout)
            .total_timeout(self.think_timeout);
        let event = GotoEvent {
            entity,
            goal,
            opts,
            id: GotoId::next(),
        };
        self.goto = Some(event.id);
        event
    }
}

/// Change the facade and send the goto that `f` returns under one ECS
/// lock, so the facade's `goto` and the engine's goal can't be seen apart.
fn update_facade<R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&mut PathfinderFacade) -> (R, Option<GotoEvent>),
) -> Option<R> {
    let mut ecs = client.ecs.write();
    let (result, goto) = {
        let mut entity = ecs.get_entity_mut(client.entity).ok()?;
        let mut facade = entity.entry::<PathfinderFacade>().or_default().into_mut();
        f(&mut facade)
    };
    if let Some(goto) = goto {
        ecs.write_message(goto);
    }
    Some(result)
}

/// Publish the `Movements` constraints into `CustomPathfinderState` so
/// [`movements_move`] sees them during the next search.
///
/// The state's own lock is taken with no ECS guard held: the search
/// thread takes it while it holds nothing, so taking it under the ECS
/// lock could cross with a search that waits for the ECS.
fn sync_constraints(client: &mineflayer_engine::Client, movements: &Movements) {
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
    pub async fn goto(&self, goal: Goal) -> Result<()> {
        let (waiter, outcome) = oneshot::channel();
        self.set_goal_inner(goal, false, Some(waiter))?;
        tokio::select! {
            outcome = outcome => match outcome {
                Ok(outcome) => outcome.map_err(Error::from),
                // the facade went away with the client entity
                Err(_) => Err(Error::NotConnected),
            },
            _ = self.bot.root.cancelled() => Err(Error::NotConnected),
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
        self.set_goal_inner(goal, dynamic, None)
    }

    fn set_goal_inner(&self, goal: Goal, dynamic: bool, waiter: Option<Waiter>) -> Result<()> {
        let client = self.client().clone();
        sync_constraints(&client, &self.movements());
        // Already satisfied — the engine would drop the goto without ever
        // storing the goal, so mirror upstream's monitor: `goal_updated`
        // then `goal_reached`, and stop the in-flight path (upstream's
        // resetPath also cleared it).
        let satisfied = !dynamic && goal.is_end(&client, client.position());
        let engine_goal =
            if satisfied {
                None
            } else {
                Some(goal.engine_goal(&client).ok_or_else(|| {
                    Error::NotFound("Goal::Follow's entity isn't loaded".to_string())
                })?)
            };
        let follow_block = follow_block(&goal);
        update_facade(&client, |f| {
            // `newGoal !== goal` — upstream's identity check doesn't reject
            // a goto when the identical goal is re-issued, so an equal goal
            // keeps the waiting gotos. `dynamic` isn't part of the
            // identity — upstream compares only the goal object.
            if !f.goal.as_ref().is_some_and(|g| g == &goal) {
                f.settle(Err(PathError::GoalChanged));
            }
            f.dynamic = dynamic;
            f.follow_block = follow_block;
            match engine_goal {
                Some(engine_goal) => {
                    f.goal = Some(goal.clone());
                    f.waiters.extend(waiter);
                    ((), Some(f.goto_event(client.entity, engine_goal)))
                }
                None => {
                    f.goal = None;
                    f.goto = None;
                    f.settle(Ok(()));
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(Ok(()));
                    }
                    ((), None)
                }
            }
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
                reason: PathResetReason::GoalUpdated,
            });
        }
        if satisfied {
            self.bot.emit_event(BotEvent::GoalReached { goal });
        }
        Ok(())
    }

    /// `bot.pathfinder.stop()` — stop navigating now (upstream clears the
    /// path immediately, so this is the `force` stop, not the
    /// mid-move-friendly `stop_pathfinding`). Emits `path_stop` here like
    /// upstream's `stop()`; the engine's `Stopped` then names a goto the
    /// facade no longer runs, so the bridge doesn't report it again.
    pub fn stop(&self) {
        let client = self.client();
        // upstream: `if (!stateGoal && path.length === 0) return` — an
        // idle stop emits nothing
        let active = !client.is_goto_target_reached()
            || with_component::<PathfinderFacade, _>(client, |f| f.goal.is_some()).unwrap_or(false);
        let stopped = with_component_mut::<PathfinderFacade, _>(client, |f| {
            f.goto = None;
            f.settle(Err(PathError::PathStopped));
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
    /// `movements_updated`) and re-paths under the new movements. The
    /// waiting `goto`s keep waiting for the same goal.
    pub fn set_movements(&self, movements: Movements) {
        let client = self.client().clone();
        sync_constraints(&client, &movements);
        let was_executing = client.is_executing_path();
        let engine_goal = self
            .goal()
            .and_then(|goal| goal.engine_goal(&client).zip(Some(goal)));
        update_facade(&client, |f| {
            f.movements = movements;
            // reissue only the goal the engine goal was built from: a
            // `set_goal` since then already sent its own goto
            let goto = match engine_goal {
                Some((engine_goal, goal))
                    if f.goto.is_some() && f.goal.as_ref().is_some_and(|g| g == &goal) =>
                {
                    Some(f.goto_event(client.entity, engine_goal))
                }
                _ => None,
            };
            ((), goto)
        });
        if was_executing {
            self.bot.emit_event(BotEvent::PathReset {
                reason: PathResetReason::MovementsUpdated,
            });
        }
    }

    /// `bot.pathfinder.thinkTimeout` — the search budget of every issued
    /// goto (default 5000 ms like upstream); also the NoPath-vs-Timeout
    /// threshold for a failed search.
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
                    let progress = crate::interact::mine_progress(
                        block.state,
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
    /// have no engine knob and are not exposed.
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
            goto: Default::default(),
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

/// The block of a [`Goal::Follow`] target, `None` for other goals or an
/// unloaded target.
fn follow_block(goal: &Goal) -> Option<BlockPos> {
    match goal {
        Goal::Follow { entity, .. } => entity
            .get_component::<Position>()
            .map(|p| BlockPos::from(**p)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
