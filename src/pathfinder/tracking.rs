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
                facade_mut(client, |f| {
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
                    facade_mut(client, |f| {
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
                    let reached = facade_mut(client, |f| {
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
            let stopped = facade_mut(client, |f| f.goal.take()).flatten();
            if let Some(goal) = stopped {
                out.push(BotEvent::PathStop { goal: Some(goal) });
            }
            out
        }
        // the engine's `PathEvent` is #[non_exhaustive]: an upstream
        // addition is dropped rather than crashing the bridge
        _ => out,
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
mod tests {
    use super::*;
    use crate::pathfinder::tests::*;

    fn path_found(path: Vec<BlockPos>, is_partial: bool, ms: u64) -> Event {
        Event::Pathfinder(PathEvent::PathFound {
            path,
            is_partial,
            elapsed: Duration::from_millis(ms),
        })
    }

    fn follow_goal_facade(client: &mineflayer_engine::Client) -> Goal {
        let goal = follow_at(client, Vec3::new(500., 64., 0.));
        facade_mut(client, |f| {
            f.goal = Some(goal.clone());
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        goal
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
        follow_goal_facade(&client);
        let mut tracker = PathTracker::default();

        // the search returns empty well inside the 5 s budget -> noPath
        let out = bridge_events(&client, &mut tracker, &path_found(Vec::new(), true, 10));
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

    /// Same failure path, but the search outlived the `think_timeout`
    /// budget -> `Timeout`, matching upstream's
    /// `results.status === 'timeout'`.
    #[test]
    fn bridge_failed_search_past_budget_reports_timeout() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        follow_goal_facade(&client);
        facade_mut(&client, |f| f.think_timeout = Duration::from_millis(10));
        let mut tracker = PathTracker::default();

        let out = bridge_events(&client, &mut tracker, &path_found(Vec::new(), true, 50));
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
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::Timeout)))
        );
    }

    /// A complete path reports `success` and leaves the goal installed
    /// (the path keeps executing; the goal clears on `goal_reached`).
    #[test]
    fn bridge_complete_path_reports_success() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        follow_goal_facade(&client);
        let mut tracker = PathTracker::default();

        let out = bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(1, 64, 0)], false, 10),
        );
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::Success,
                    ..
                }
            )),
            "expected a success path_update, got {out:?}"
        );
        assert!(facade(&client, |f| f.goal.is_some()).unwrap_or(false));
    }

    /// A partial path reports `partial` and leaves the goal installed.
    #[test]
    fn bridge_partial_path_reports_partial() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        follow_goal_facade(&client);
        let mut tracker = PathTracker::default();

        let out = bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(1, 64, 0)], true, 10),
        );
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

    /// Upstream's `thinkTimeout` is a cumulative promise budget: a goal
    /// that keeps producing partial paths without ever completing must
    /// report `Timeout` once its searches total the budget — the engine
    /// would otherwise repath the unreachable frontier forever. The
    /// facade goal is upstream's surviving `stateGoal` — it stays
    /// installed with the outcome recorded for a pending `goto`, and the
    /// report is latched (it can't re-fire while the goal repaths).
    #[test]
    fn bridge_cumulative_search_budget_reports_timeout() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        follow_goal_facade(&client);
        facade_mut(&client, |f| f.think_timeout = Duration::from_millis(10));
        let mut tracker = PathTracker::default();
        let has_timeout = |out: &[BotEvent]| {
            out.iter().any(|e| {
                matches!(
                    e,
                    BotEvent::PathUpdate {
                        status: PathStatus::Timeout,
                        ..
                    }
                )
            })
        };

        // three partial repaths of 4 ms each cross the 10 ms budget
        assert!(!has_timeout(&bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(1, 64, 0)], true, 4)
        )));
        assert!(!has_timeout(&bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(2, 64, 0)], true, 4)
        )));
        let out = bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(3, 64, 0)], true, 4),
        );
        assert!(
            has_timeout(&out),
            "expected the timeout report, got {out:?}"
        );
        assert_eq!(
            facade(&client, |f| (f.goal.is_some(), f.last_failure)),
            Some((true, Some(PathError::Timeout))),
            "the surviving stateGoal stays installed"
        );

        // latched — a further partial doesn't re-fire
        assert!(!has_timeout(&bridge_events(
            &client,
            &mut tracker,
            &path_found(vec![BlockPos::new(4, 64, 0)], true, 4)
        )));
    }

    /// N-1: the cumulative budget is promise-level. A `dynamic` goal has
    /// no promise to settle and upstream keeps `stateGoal` repathing
    /// after every `timeout` status — the report fires once, and the
    /// goal/watcher survive.
    #[test]
    fn bridge_dynamic_goal_survives_budget_expiry() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        let goal = follow_at(&client, Vec3::new(500., 64., 0.));
        let watcher_cancel = Arc::new(AtomicBool::new(false));
        facade_mut(&client, |f| {
            f.goal = Some(goal);
            f.dynamic = true;
            f.goal_gen = 1;
            f.set_gen = 1;
            f.think_timeout = Duration::from_millis(10);
            f.watcher_cancel = Some(watcher_cancel.clone());
        });
        let mut tracker = PathTracker::default();

        let mut timed_out = false;
        for i in 0..5 {
            let out = bridge_events(
                &client,
                &mut tracker,
                &path_found(vec![BlockPos::new(i, 64, 0)], true, 4),
            );
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
        assert!(timed_out, "the timeout status must report");
        assert_eq!(
            facade(&client, |f| (
                f.goal.is_some(),
                f.last_failure,
                f.watcher_cancel.is_some()
            )),
            Some((true, Some(PathError::Timeout), true)),
            "dynamic goal + watcher must survive the budget"
        );
        assert!(!watcher_cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// A satisfied goal reports `goal_reached` and clears the facade goal
    /// — with no `path_update`.
    #[test]
    fn bridge_satisfied_goal_reports_reached() {
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

        let out = bridge_events(
            &client,
            &mut tracker,
            &Event::Pathfinder(PathEvent::GoalReached),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::GoalReached { .. })),
            "expected goal_reached, got {out:?}"
        );
        assert!(!out.iter().any(|e| matches!(e, BotEvent::PathUpdate { .. })));
        assert!(facade(&client, |f| f.goal.is_none()).unwrap_or(false));
    }

    /// A `dynamic` goal's reach is the watcher's business — the facade
    /// goal stays and nothing is reported, matching upstream.
    #[test]
    fn bridge_dynamic_goal_reached_stays_installed() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        facade_mut(&client, |f| {
            f.goal = Some(Goal::Y { y: 64 });
            f.dynamic = true;
            f.goal_gen = 1;
            f.set_gen = 1;
        });
        let mut tracker = PathTracker::default();

        let out = bridge_events(
            &client,
            &mut tracker,
            &Event::Pathfinder(PathEvent::GoalReached),
        );
        assert!(out.is_empty(), "dynamic reach must be silent, got {out:?}");
        assert!(facade(&client, |f| f.goal.is_some()).unwrap_or(false));
    }

    /// A stop the port initiated (`stop()`, the follow driver) cleared
    /// the facade goal first — the engine's `Stopped` translation must
    /// stay silent so the port's own `path_stop` is the only report.
    #[test]
    fn bridge_stopped_without_a_facade_goal_is_silent() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        let mut tracker = PathTracker::default();
        let out = bridge_events(
            &client,
            &mut tracker,
            &Event::Pathfinder(PathEvent::Stopped),
        );
        assert!(out.is_empty(), "expected silence, got {out:?}");
    }

    /// A stop the port didn't initiate (a world change, an engine-
    /// internal drop) still finds the facade goal — take it and report
    /// `path_stop`, like upstream's surviving `path_stop`.
    #[test]
    fn bridge_stopped_with_a_facade_goal_reports_path_stop() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        follow_goal_facade(&client);
        let mut tracker = PathTracker::default();
        let out = bridge_events(
            &client,
            &mut tracker,
            &Event::Pathfinder(PathEvent::Stopped),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::PathStop { goal: Some(_) })),
            "expected path_stop with the goal, got {out:?}"
        );
        assert!(facade(&client, |f| f.goal.is_none()).unwrap_or(false));
    }

    /// Non-pathfinder events stay inert.
    #[test]
    fn bridge_ignores_other_events() {
        let client = watcher_client((
            Position::new(Vec3::new(0., 64., 0.)),
            pathfinder::Pathfinder::default(),
        ));
        let mut tracker = PathTracker::default();
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
    }
}
