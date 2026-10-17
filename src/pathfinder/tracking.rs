//! Path events: the per-tick diff of the engine `Pathfinder`/`ExecutingPath`
//! components and the facade, turned into `path_update`/`goal_reached`/
//! `path_stop` events (see the module notes in `pathfinder` on wedges and
//! the cumulative search budget).

use super::*;

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
    /// drops inside one GameTick gap — the engine runs the whole goto chain
    /// per Update (~17 ms) while snapshots land per GameTick (50 ms) —
    /// so edge detection can't see it; the persistent wedge is the only
    /// observable signature. Armed under [`WEDGE_GRACE`] so a
    /// just-written `GotoEvent` can't false-positive. The timestamp
    /// survives install/calc flicker — only satisfaction, execution, or
    /// a re-issue disarms it, so a watcher re-issue inside the grace
    /// window can't starve the wedge by re-hiding each failure (N-4).
    wedge_since: Option<Instant>,
    /// The `goal_gen` the search bookkeeping below belongs to — a
    /// `set_goal`/`set_movements` re-issue resets it (new promise, new
    /// budget).
    tracked_gen: u64,
    /// Observed search time accumulated for `tracked_gen` since the
    /// last complete path. Upstream's `thinkTimeout` bounds one A*
    /// end-to-end; the engine splits it into a fresh `max_timeout` per
    /// repath, so the budget has to span them — a goal whose searches
    /// total `think_timeout` without producing a complete path rejects
    /// `Timeout` even while partial progress keeps coming (the common
    /// unreachable-but-approachable case). Hidden searches — the ones
    /// living and dying inside the tick gap — are unmetered here; the
    /// wedge bounds them (N-7).
    search_spent: Duration,
    /// The goal's currently-executing path is a complete (non-partial)
    /// one — while that holds, the budget is disarmed; when it ends a
    /// fresh repath batch accumulates from zeroed `search_spent` (a
    /// success-then-perpetual-partial goal still rejects, N-2).
    saw_success: bool,
    /// This batch's cumulative budget already produced its `timeout`
    /// report — latched so a blown budget can't re-fire the status
    /// every tick while the (surviving) goal keeps repathing. Re-arms
    /// when a complete path installs or the goal is re-issued.
    timeout_reported: bool,
}

/// Everything the diff needs from the ECS, extracted under one read guard
/// (extract, drop, then diff — never hold the guard across another lock).
pub(super) struct PathSnapshot {
    /// `Pathfinder.goal` — kept for the success check when it clears.
    ecs_goal: Option<Arc<dyn EngineGoal>>,
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

/// Called by the bridge for every engine [`Event`]; emits the pathfinder
/// `BotEvent`s derivable from component state. Only `Tick` carries a
/// signal — see the module docs for the diff rules.
pub(crate) fn bridge_events(
    client: &mineflayer_engine::Client,
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
        tracker.timeout_reported = false;
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
    // upstream's thinkTimeout bounds one A* end-to-end; the engine splits it
    // into a fresh search per repath, so elapsed accumulates across them
    if let Some(elapsed) = calc_elapsed {
        tracker.search_spent += elapsed;
    }
    // a freshly-installed complete path establishes the goal: the
    // budget restarts from zero for whatever repaths follow it —
    // upstream's next timed-out recalc still rejects a pending goto —
    // and the `timeout` latch re-arms for the new batch. While the
    // complete path keeps executing the budget stays disarmed; a
    // partial path, an idle goal, or a mid-search snapshot ends the
    // establishment so post-success partial loops still meet a
    // terminal status (N-2).
    if now.executing && !now.is_partial {
        if !tracker.saw_success {
            tracker.saw_success = true;
            tracker.search_spent = Duration::ZERO;
            tracker.timeout_reported = false;
        }
    } else {
        tracker.saw_success = false;
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
    // 'timeout' (the engine would repath a partial frontier forever)
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
    // can raycast (ReachBlockPosGoal) and shouldn't run per tick. The
    // budget term is gated on the latch — once the timeout has been
    // reported, the surviving goal doesn't keep paying raycast ticks.
    let satisfied = (calc_finished
        || goal_cleared
        || wedge_candidate
        || (budget_exceeded && !tracker.timeout_reported))
        && now
            .facade_goal
            .as_ref()
            .is_some_and(|g| now.pos.is_some_and(|pos| g.is_end(client, pos)));

    // the wedge must persist past the grace window — a just-written
    // `GotoEvent` shows the same signature for the ~17 ms until the
    // next Update drains it, so flagging immediately would
    // false-positive. The timestamp survives install/calc flicker: a
    // watcher re-issue inside the window re-hides each hidden drop,
    // and resetting on `ecs_goal.is_some()` would starve the wedge
    // forever under a moving unreachable target (N-4) — only real
    // progress (satisfaction or an executing path) disarms it.
    tracker.wedge_since = if wedge_candidate && !satisfied {
        tracker.wedge_since.or_else(|| Some(Instant::now()))
    } else if satisfied || now.executing {
        None
    } else {
        tracker.wedge_since
    };
    let wedged = wedge_candidate
        && tracker
            .wedge_since
            .is_some_and(|t| t.elapsed() >= WEDGE_GRACE);

    // The engine drops its goal on an empty result only when
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
    // the cumulative promise budget expiring reports 'timeout' — but
    // unlike a dead search, the goal is upstream's surviving
    // `stateGoal`: a plain/dynamic `set_goal` has no promise to settle
    // and upstream keeps repathing after every `timeout` status, and
    // even for `goto` the reject is promise-only while
    // `monitorMovement` keeps walking. So nothing is cleared or
    // cancelled — `last_failure` is recorded for a pending `goto` and
    // the status is emitted, latched once per batch so a blown budget
    // can't re-fire every tick (N-1).
    let promise_expired =
        search_failure.is_none() && budget_exceeded && !satisfied && !tracker.timeout_reported;

    // `path_update` — a path got installed (possibly merged mid-run by a
    // repath) or a calculation finished while something owned it: a
    // facade goal, or a path it just installed. `status` mirrors
    // upstream's `results.status`. A bare finish with neither — the
    // `stop()`/aborted-calc shape — stays silent, matching upstream's
    // aborted `astarContext` which produces no `path_update` at all.
    // The executing→executing arm catches queued-path merges: walking
    // only pops nodes, so a path that *grows* is a repath result
    // (upstream emits `path_update` per result, N-6).
    if (!prev.executing && now.executing)
        || (calc_finished && (now.executing || now.facade_goal.is_some()))
        || (search_failure.is_some() && !calc_finished)
        || (prev.executing && now.executing && now.path.len() > prev.path.len())
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
            // a calculation whose result landed with no path — the engine's
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

    // promise-expired report — the facade goal is upstream's surviving
    // `stateGoal` and stays (the engine's ECS goal/partial path is alive and
    // keeps repathing; `monitorMovement` survives a 'timeout' reject
    // there too). The watcher stays with it. Only the outcome is
    // recorded so a pending `goto` can reject `Timeout` — that's the
    // entire promise-level effect upstream's 'timeout' status has.
    if promise_expired && now.facade_gen == prev.facade_gen {
        tracker.timeout_reported = true;
        facade_mut(client, |f| {
            if f.goal_gen == now.facade_gen {
                f.last_failure = Some(PathError::Timeout);
            }
        });
        out.push(BotEvent::PathUpdate {
            path: now.path.clone(),
            is_partial: now.is_partial,
            status: PathStatus::Timeout,
        });
    }

    // `goal_reached` / `path_stop` / failure-clear — the engine cleared its
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
                client.stop_pathfinding();
                facade_mut(client, |f| {
                    if f.goal_gen == now.facade_gen {
                        f.last_failure = search_failure;
                        cancel_watcher(f);
                        f.goal = None;
                    }
                });
            } else if goal_cleared {
                // the same goal was abandoned without reaching (world
                // change, the engine-internal stop). A goal_gen bump means the
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
pub(super) fn snapshot(client: &mineflayer_engine::Client) -> PathSnapshot {
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
    use crate::pathfinder::tests::*;

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
    /// thinkTimeout aborts an in-flight A* the same way — while
    /// `stateGoal` stays installed until the search's own end confirms
    /// it dead.
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
        // the reject is promise-level: `stateGoal` stays installed with
        // the outcome recorded for a pending `goto`
        assert_eq!(
            facade(&client, |f| (f.goal.is_some(), f.last_failure)),
            Some((true, Some(PathError::Timeout)))
        );
        // the search's own timeout then kills it (the engine's max_timeout
        // drops the goal) — the observed edge reports its result and
        // clears the dead goal like any failed search
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
            "the dead search's finish reports its timeout, got {out:?}"
        );
        assert_eq!(
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::Timeout)))
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
    /// report `Timeout` once its searches total the budget — the engine
    /// would otherwise repath the unreachable frontier forever. The
    /// facade goal is upstream's surviving `stateGoal` — it stays
    /// installed with the outcome recorded for a pending `goto`, and
    /// the partial path keeps walking like `monitorMovement` does.
    #[test]
    fn bridge_cumulative_search_budget_reports_timeout() {
        use std::collections::VecDeque;
        fn noop(_: moves::ExecuteCtx) {}
        fn unreached(_: moves::IsReachedCtx) -> bool {
            false
        }
        let partial_path = |client: &mineflayer_engine::Client, target| {
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
            if has_timeout(&bridge_events(&client, &mut tracker, &Event::Tick)) {
                timed_out = true;
                break;
            }
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
            if has_timeout(&bridge_events(&client, &mut tracker, &Event::Tick)) {
                timed_out = true;
                break;
            }
        }
        assert!(timed_out, "cumulative search spend never rejected");
        // upstream's `stateGoal` survives a 'timeout' — the facade goal
        // stays installed (the report is promise-level), the outcome is
        // recorded for a pending `goto`, and the partial path keeps
        // walking like `monitorMovement` does
        assert_eq!(
            facade(&client, |f| (f.goal.is_some(), f.last_failure)),
            Some((true, Some(PathError::Timeout))),
            "facade goal must survive with the timeout recorded"
        );
        assert!(client.is_executing_path());
        // and the status latches: the blown budget can't re-fire every
        // tick while the goal keeps repathing
        assert!(
            bridge_events(&client, &mut tracker, &Event::Tick)
                .iter()
                .all(|e| !matches!(
                    e,
                    BotEvent::PathUpdate {
                        status: PathStatus::Timeout,
                        ..
                    }
                ))
        );
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

        // what stop() does: clear the facade goal, then the ECS
        // goal/calc (facade-first so the intermediate state is an
        // ownerless installed goal — silent in every arm)
        facade_mut(&client, |f| f.goal = None);
        // a bridge tick in this window sees an ownerless installed goal
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        client.force_stop_pathfinding();
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            !out.iter().any(|e| matches!(e, BotEvent::PathUpdate { .. })),
            "an aborted calc must not emit path_update, got {out:?}"
        );
    }

    /// N-4: a watcher re-issue that lands on a snapshot inside the
    /// grace window must not reset the wedge clock — the install
    /// flicker keeps the arming alive, so a goal whose searches die
    /// hidden still meets its `noPath` verdict instead of degenerating
    /// into a `path_stop` (or, pathological, starvation).
    #[test]
    fn bridge_wedge_survives_install_flicker() {
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
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        // wedge arms: facade goal set, ECS idle, goal unmet
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // the flicker — a re-issued install lands on snapshots (no calc
        // ever observed); the wedge clock must keep running
        set_goal_installed(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        std::thread::sleep(WEDGE_GRACE + Duration::from_millis(30));
        // still installed — no verdict yet (the wedge is an idle check)
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());

        // the install drops; the armed clock is already past the grace
        // window → the dead search reports `noPath`, not `path_stop`
        set_goal_installed(&client, false);
        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::NoPath,
                    ..
                }
            )),
            "expected noPath after the flicker, got {out:?}"
        );
        assert!(
            !out.iter().any(|e| matches!(e, BotEvent::PathStop { .. })),
            "a dead search is a failure, not a stop"
        );
        assert_eq!(
            facade(&client, |f| (f.goal.is_none(), f.last_failure)),
            Some((true, Some(PathError::NoPath)))
        );
    }

    /// N-1: the cumulative budget is promise-level. A `dynamic` goal has
    /// no promise to settle and upstream keeps `stateGoal` repathing
    /// after every `timeout` status — the report fires once, latched,
    /// and the goal/watcher survive.
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
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        set_searching(&client, true);
        assert!(bridge_events(&client, &mut tracker, &Event::Tick).is_empty());
        std::thread::sleep(Duration::from_millis(30));

        let out = bridge_events(&client, &mut tracker, &Event::Tick);
        assert!(
            out.iter().any(|e| matches!(
                e,
                BotEvent::PathUpdate {
                    status: PathStatus::Timeout,
                    ..
                }
            )),
            "the timeout status still reports, got {out:?}"
        );
        // upstream's `stateGoal` survives — the goal and its watcher
        // stay (a `goto`'s promise rejects off `last_failure`, nothing
        // else changes)
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
        // latched — no re-fire while the goal keeps repathing
        assert!(
            bridge_events(&client, &mut tracker, &Event::Tick)
                .iter()
                .all(|e| !matches!(
                    e,
                    BotEvent::PathUpdate {
                        status: PathStatus::Timeout,
                        ..
                    }
                ))
        );
    }
}
