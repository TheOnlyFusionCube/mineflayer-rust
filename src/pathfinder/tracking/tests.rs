use super::*;
use crate::pathfinder::tests::*;
use mineflayer_engine::pathfinder;

const GOTO: GotoId = GotoId(41);
const STALE: GotoId = GotoId(40);

fn path_found(goto: GotoId, path: Vec<BlockPos>, is_partial: bool, ms: u64) -> Event {
    Event::Pathfinder(PathEvent::PathFound {
        goto,
        path,
        is_partial,
        elapsed: Duration::from_millis(ms),
    })
}

fn client() -> mineflayer_engine::Client {
    pathfinder_client((
        Position::new(Vec3::new(0., 64., 0.)),
        pathfinder::Pathfinder::default(),
    ))
}

/// Install `goal` as running as [`GOTO`], with one waiting `goto`.
fn install(
    client: &mineflayer_engine::Client,
    goal: Goal,
    dynamic: bool,
) -> oneshot::Receiver<std::result::Result<(), PathError>> {
    let (waiter, outcome) = oneshot::channel();
    with_component_mut::<PathfinderFacade, _>(client, |f| {
        f.goal = Some(goal);
        f.dynamic = dynamic;
        f.goto = Some(GOTO);
        f.waiters.push(waiter);
    });
    outcome
}

fn facade<R>(client: &mineflayer_engine::Client, f: impl FnOnce(&PathfinderFacade) -> R) -> R {
    with_component::<PathfinderFacade, _>(client, f).unwrap()
}

fn has_status(out: &[BotEvent], want: PathStatus) -> bool {
    out.iter()
        .any(|e| matches!(e, BotEvent::PathUpdate { status, .. } if *status == want))
}

/// A failed search reports its status (upstream's `results.status`),
/// clears the goal, and settles the waiting `goto`, with no `path_stop`.
#[test]
fn a_failed_search_reports_nopath_and_clears_the_goal() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);

    let out = bridge_events(&client, &path_found(GOTO, Vec::new(), true, 10));
    assert!(has_status(&out, PathStatus::NoPath), "{out:?}");
    assert!(!out.iter().any(|e| matches!(e, BotEvent::PathStop { .. })));
    assert!(facade(&client, |f| f.goal.is_none() && f.goto.is_none()));
    assert_eq!(outcome.try_recv(), Ok(Err(PathError::NoPath)));
}

/// The same failure past `think_timeout` is upstream's `timeout`.
#[test]
fn a_failed_search_past_the_budget_reports_timeout() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);
    with_component_mut::<PathfinderFacade, _>(&client, |f| {
        f.think_timeout = Duration::from_millis(10)
    });

    let out = bridge_events(&client, &path_found(GOTO, Vec::new(), true, 50));
    assert!(has_status(&out, PathStatus::Timeout), "{out:?}");
    assert_eq!(outcome.try_recv(), Ok(Err(PathError::Timeout)));
}

/// Complete and partial paths report their status and keep the goal.
#[test]
fn complete_and_partial_paths_keep_the_goal() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);
    let step = vec![BlockPos::new(1, 64, 0)];

    let out = bridge_events(&client, &path_found(GOTO, step.clone(), false, 10));
    assert!(has_status(&out, PathStatus::Success), "{out:?}");
    let out = bridge_events(&client, &path_found(GOTO, step, true, 10));
    assert!(has_status(&out, PathStatus::Partial), "{out:?}");
    assert!(facade(&client, |f| f.goal.is_some()));
    assert!(outcome.try_recv().is_err(), "the goto still waits");
}

/// The engine's `Timeout` (the goal's searches used `think_timeout`)
/// rejects the waiting `goto` and keeps the goal: upstream's surviving
/// `stateGoal`.
#[test]
fn the_engine_timeout_settles_the_goto_and_keeps_the_goal() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);
    let path = vec![BlockPos::new(3, 64, 0)];

    let out = bridge_events(
        &client,
        &Event::Pathfinder(PathEvent::Timeout {
            goto: GOTO,
            path: path.clone(),
        }),
    );
    assert!(
        out.iter().any(|e| matches!(
            e,
            BotEvent::PathUpdate { status: PathStatus::Timeout, path: p, .. } if *p == path
        )),
        "{out:?}"
    );
    assert_eq!(outcome.try_recv(), Ok(Err(PathError::Timeout)));
    assert!(facade(&client, |f| f.goal.is_some() && f.goto == Some(GOTO)));
}

/// An event of a goto the goal no longer runs as changes nothing.
#[test]
fn events_of_a_stale_goto_are_ignored() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);
    for event in [
        path_found(STALE, Vec::new(), true, 10),
        Event::Pathfinder(PathEvent::GoalReached { goto: STALE }),
        Event::Pathfinder(PathEvent::Stopped { goto: STALE }),
    ] {
        assert!(bridge_events(&client, &event).is_empty());
    }
    assert!(facade(&client, |f| f.goal.is_some()));
    assert!(outcome.try_recv().is_err());
}

/// A reached goal reports `goal_reached`, clears the goal, and resolves
/// the `goto`.
#[test]
fn a_reached_goal_reports_goal_reached() {
    let client = client();
    let mut outcome = install(&client, Goal::Y { y: 64 }, false);

    let out = bridge_events(
        &client,
        &Event::Pathfinder(PathEvent::GoalReached { goto: GOTO }),
    );
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::GoalReached { .. })),
        "{out:?}"
    );
    assert!(facade(&client, |f| f.goal.is_none()));
    assert_eq!(outcome.try_recv(), Ok(Ok(())));
}

/// A `dynamic` goal never completes: its reach is silent, and the goal
/// waits at its target with no goto.
#[test]
fn a_dynamic_goal_waits_at_its_target() {
    let client = client();
    install(&client, Goal::Y { y: 64 }, true);

    let out = bridge_events(
        &client,
        &Event::Pathfinder(PathEvent::GoalReached { goto: GOTO }),
    );
    assert!(out.is_empty(), "{out:?}");
    assert!(facade(&client, |f| f.goal.is_some() && f.goto.is_none()));
    // still met: the tick sends nothing
    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 0);
}

/// A `dynamic` goal that stops holding goes to the engine again.
#[test]
fn a_dynamic_goal_that_stops_holding_is_sent_again() {
    let client = client();
    install(&client, Goal::Y { y: 70 }, true);
    with_component_mut::<PathfinderFacade, _>(&client, |f| f.goto = None);

    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 1);
    let goto = facade(&client, |f| f.goto);
    assert!(goto.is_some_and(|g| g != GOTO));
    // running: the next tick sends nothing
    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 1);
}

/// A follow goal goes to the engine again when its target moves to
/// another block, and not while the target stays in its block.
#[test]
fn a_follow_goal_follows_its_target_by_block() {
    let client = client();
    let target = client
        .ecs
        .write()
        .spawn(Position::new(Vec3::new(500.5, 64., 0.5)))
        .id();
    install(
        &client,
        Goal::Follow {
            entity: client.entity_ref_for(target),
            range: 1.,
        },
        true,
    );

    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 1, "the first block");
    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 1, "the same block");
    *client.ecs.write().get_mut::<Position>(target).unwrap() =
        Position::new(Vec3::new(503.5, 64., 0.5));
    bridge_events(&client, &Event::Tick);
    assert_eq!(goto_messages(&client), 2, "a new block");
}

/// A follow target that despawns ends the goal with `path_stop`, as
/// upstream's `isValid()` failure does.
#[test]
fn a_despawned_follow_target_stops_the_goal() {
    let client = client();
    let target = client
        .ecs
        .write()
        .spawn(Position::new(Vec3::new(500., 64., 0.)))
        .id();
    let mut outcome = install(
        &client,
        Goal::Follow {
            entity: client.entity_ref_for(target),
            range: 1.,
        },
        false,
    );
    client.ecs.write().despawn(target);

    let out = bridge_events(&client, &Event::Tick);
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::PathStop { goal: Some(_) })),
        "{out:?}"
    );
    assert!(facade(&client, |f| f.goal.is_none()));
    assert_eq!(outcome.try_recv(), Ok(Err(PathError::PathStopped)));
}

/// A stop from inside the engine (a world change) reports `path_stop`.
#[test]
fn an_engine_stop_reports_path_stop() {
    let client = client();
    let mut outcome = install(&client, follow_at(&client, Vec3::new(500., 64., 0.)), false);

    let out = bridge_events(
        &client,
        &Event::Pathfinder(PathEvent::Stopped { goto: GOTO }),
    );
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::PathStop { goal: Some(_) })),
        "{out:?}"
    );
    assert_eq!(outcome.try_recv(), Ok(Err(PathError::PathStopped)));
}
