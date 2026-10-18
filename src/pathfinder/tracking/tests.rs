use super::*;
use crate::pathfinder::tests::*;

fn path_found(path: Vec<BlockPos>, is_partial: bool, ms: u64) -> Event {
    Event::Pathfinder(PathEvent::PathFound {
        goto: Default::default(),
        path,
        is_partial,
        elapsed: Duration::from_millis(ms),
    })
}

fn follow_goal_facade(client: &mineflayer_engine::Client) -> Goal {
    let goal = follow_at(client, Vec3::new(500., 64., 0.));
    with_component_mut::<PathfinderFacade, _>(client, |f| {
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
        with_component::<PathfinderFacade, _>(&client, |f| (f.goal.is_none(), f.last_failure)),
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
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
        f.think_timeout = Duration::from_millis(10)
    });
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
        with_component::<PathfinderFacade, _>(&client, |f| (f.goal.is_none(), f.last_failure)),
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
    assert!(with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_some()).unwrap_or(false));
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
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
        f.think_timeout = Duration::from_millis(10)
    });
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
        with_component::<PathfinderFacade, _>(&client, |f| (f.goal.is_some(), f.last_failure)),
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

/// The cumulative budget is promise-level. A `dynamic` goal has
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
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
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
        with_component::<PathfinderFacade, _>(&client, |f| (
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
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
        // a YGoal the bot is already standing on
        f.goal = Some(Goal::Y { y: 64 });
        f.goal_gen = 1;
        f.set_gen = 1;
    });
    let mut tracker = PathTracker::default();

    let out = bridge_events(
        &client,
        &mut tracker,
        &Event::Pathfinder(PathEvent::GoalReached {
            goto: Default::default(),
        }),
    );
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::GoalReached { .. })),
        "expected goal_reached, got {out:?}"
    );
    assert!(!out.iter().any(|e| matches!(e, BotEvent::PathUpdate { .. })));
    assert!(with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_none()).unwrap_or(false));
}

/// A `dynamic` goal's reach is the watcher's business — the facade
/// goal stays and nothing is reported, matching upstream.
#[test]
fn bridge_dynamic_goal_reached_stays_installed() {
    let client = watcher_client((
        Position::new(Vec3::new(0., 64., 0.)),
        pathfinder::Pathfinder::default(),
    ));
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
        f.goal = Some(Goal::Y { y: 64 });
        f.dynamic = true;
        f.goal_gen = 1;
        f.set_gen = 1;
    });
    let mut tracker = PathTracker::default();

    let out = bridge_events(
        &client,
        &mut tracker,
        &Event::Pathfinder(PathEvent::GoalReached {
            goto: Default::default(),
        }),
    );
    assert!(out.is_empty(), "dynamic reach must be silent, got {out:?}");
    assert!(with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_some()).unwrap_or(false));
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
        &Event::Pathfinder(PathEvent::Stopped {
            goto: Default::default(),
        }),
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
        &Event::Pathfinder(PathEvent::Stopped {
            goto: Default::default(),
        }),
    );
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::PathStop { goal: Some(_) })),
        "expected path_stop with the goal, got {out:?}"
    );
    assert!(with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_none()).unwrap_or(false));
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
