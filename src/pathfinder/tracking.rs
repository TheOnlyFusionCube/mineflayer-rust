//! Path events: translate the engine's [`Event::Pathfinder`] stream into
//! `path_update`/`goal_reached`/`path_stop`. The engine reports every
//! search's outcome (the node list, partiality, and elapsed time), goal
//! satisfaction, and stops, so nothing in this module diffs components
//! anymore.
//!
//! One piece of bookkeeping survives, mirroring upstream: the cumulative
//! search budget. Upstream's `thinkTimeout` bounds one goal end-to-end,
//! but the engine gives each repath a fresh `max_timeout`, so the budget
//! is tracked across the `PathFound` events of one goal issue. The reject
//! is promise-level like upstream's: the facade goal is the surviving
//! `stateGoal` and stays installed (watchers and partial paths keep
//! running); only `last_failure` records the outcome so a pending `goto`
//! can settle. A search that ends with no path at all is a failure —
//! cleared through the same path `stop()` uses, reported as the status
//! (`noPath`/`timeout`), never as a `path_stop`.

use super::*;

/// Per-goal search-budget state — see [`bridge_events`].
#[derive(Default)]
pub(crate) struct PathTracker {
    /// The `goal_gen` the bookkeeping belongs to — a `set_goal`/
    /// `set_movements` re-issue resets it (new promise, new budget).
    tracked_gen: u64,
    /// Search time accumulated for `tracked_gen` since the last complete
    /// path.
    search_spent: Duration,
    /// The goal's latest search produced a complete path — the budget is
    /// disarmed until a partial result ends that establishment.
    saw_success: bool,
    /// This batch's budget already produced its `timeout` report —
    /// latched so a blown budget can't re-fire while the goal keeps
    /// repathing.
    timeout_reported: bool,
}

/// Called by the bridge for every engine [`Event`]; translates
/// [`Event::Pathfinder`] into the pathfinder `BotEvent`s.
pub(crate) fn bridge_events(
    client: &mineflayer_engine::Client,
    tracker: &mut PathTracker,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Pathfinder(path_event) = event else {
        return vec![];
    };
    let (facade_goal, facade_gen, dynamic, think_timeout) = facade_context(client);
    // the bookkeeping is scoped to one goal issue — a re-issue restarts
    // the promise (fresh goal, fresh budget, fresh latch)
    if facade_gen != tracker.tracked_gen {
        tracker.tracked_gen = facade_gen;
        tracker.search_spent = Duration::ZERO;
        tracker.saw_success = false;
        tracker.timeout_reported = false;
    }
    let mut out = Vec::new();

    match path_event {
        PathEvent::PathFound {
            path,
            is_partial,
            elapsed,
        } => {
            if !is_partial {
                // a complete path establishes the goal: the budget
                // restarts from zero for whatever repaths follow it
                tracker.saw_success = true;
                tracker.search_spent = Duration::ZERO;
                tracker.timeout_reported = false;
                out.push(BotEvent::PathUpdate {
                    path: path.clone(),
                    is_partial: false,
                    status: PathStatus::Success,
                });
            } else if !path.is_empty() {
                // a partial path keeps walking (upstream's 'partial');
                // the budget accumulates across the repaths below
                tracker.saw_success = false;
                out.push(BotEvent::PathUpdate {
                    path: path.clone(),
                    is_partial: true,
                    status: PathStatus::Partial,
                });
            } else if facade_goal.is_some() {
                // an empty result: the space was exhausted (`noPath`) or
                // the per-search budget ran out (`timeout`), classified
                // by duration like upstream's `results.status`. Clear
                // through the same path `stop()` uses, record the outcome
                // for pending gotos, and retire the watcher so a
                // `dynamic` goal can't resurrect the dead search into a
                // busy re-path loop. No `path_stop`: the status is the
                // whole report.
                let error = if *elapsed >= think_timeout {
                    PathError::Timeout
                } else {
                    PathError::NoPath
                };
                client.stop_pathfinding();
                with_component_mut::<PathfinderFacade, _>(client, |f| {
                    if f.goal_gen == facade_gen {
                        f.last_failure = Some(error);
                        cancel_watcher(f);
                        f.goal = None;
                    }
                });
                out.push(BotEvent::PathUpdate {
                    path: Vec::new(),
                    is_partial: true,
                    status: match error {
                        PathError::Timeout => PathStatus::Timeout,
                        _ => PathStatus::NoPath,
                    },
                });
                return out;
            } else {
                // no facade goal owns this search — the aborted-context
                // shape stays silent, like upstream
                return out;
            }

            // the cumulative promise budget: partial repaths totaling
            // `think_timeout` without a complete path report `timeout`
            // once. The facade goal is upstream's surviving `stateGoal`
            // and stays; `last_failure` records the outcome for a
            // pending `goto`, and the latch stops a blown budget from
            // re-firing while the goal keeps repathing.
            if facade_goal.is_some() && !tracker.saw_success {
                tracker.search_spent += *elapsed;
                if tracker.search_spent >= think_timeout && !tracker.timeout_reported {
                    tracker.timeout_reported = true;
                    with_component_mut::<PathfinderFacade, _>(client, |f| {
                        if f.goal_gen == facade_gen {
                            f.last_failure = Some(PathError::Timeout);
                        }
                    });
                    out.push(BotEvent::PathUpdate {
                        path: path.clone(),
                        is_partial: *is_partial,
                        status: PathStatus::Timeout,
                    });
                }
            }
            out
        }
        PathEvent::GoalReached => {
            // the engine satisfied a goal. Take the facade goal only when
            // it is still ours and its success check holds now (a newer
            // `set_goal` may have replaced it while the event was in
            // flight); `dynamic` goals stay — the watcher re-paths when
            // they stop holding, matching upstream.
            if !dynamic {
                let satisfied_now = facade_goal
                    .as_ref()
                    .zip(self_position(client))
                    .is_some_and(|(goal, pos)| goal.is_end(client, pos));
                if satisfied_now {
                    let reached = with_component_mut::<PathfinderFacade, _>(client, |f| {
                        if f.goal_gen == facade_gen {
                            f.goal.take()
                        } else {
                            None
                        }
                    })
                    .flatten();
                    if let Some(goal) = reached {
                        out.push(BotEvent::GoalReached { goal });
                    }
                }
            }
            out
        }
        PathEvent::Stopped => {
            // the engine abandoned a goal. A stop the port initiated
            // (`stop()`, a `set_goal` on a satisfied goal, the follow
            // driver's despawn) already cleared the facade goal and
            // reported itself; only a stop from elsewhere (a world
            // change, an engine-internal drop) still finds the facade
            // goal present — take it and report.
            let stopped =
                with_component_mut::<PathfinderFacade, _>(client, |f| f.goal.take()).flatten();
            if let Some(goal) = stopped {
                out.push(BotEvent::PathStop { goal: Some(goal) });
            }
            out
        }
    }
}

/// Read the facade's goal context in one guard.
fn facade_context(client: &mineflayer_engine::Client) -> (Option<Goal>, u64, bool, Duration) {
    let ecs = client.ecs.read();
    ecs.get_entity(client.entity)
        .ok()
        .and_then(|entity| entity.get::<PathfinderFacade>())
        .map_or((None, 0, false, DEFAULT_THINK_TIMEOUT), |f| {
            (f.goal.clone(), f.goal_gen, f.dynamic, f.think_timeout)
        })
}

fn self_position(client: &mineflayer_engine::Client) -> Option<Vec3> {
    let ecs = client.ecs.read();
    let entity = ecs.get_entity(client.entity).ok()?;
    entity.get::<Position>().map(|p| **p)
}

#[cfg(test)]
mod tests;
