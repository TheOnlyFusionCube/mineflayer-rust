use super::*;

/// A client whose ECS carries `TickBroadcast` (watchers park on it)
/// plus the pathfinder message resources, so a watcher's
/// `start_goto_with_opts` lands in `Messages<GotoEvent>` where the
/// test can count it. No systems run — ticks arrive only via
/// [`crate::test_util::tick_sender`].
pub(super) fn watcher_client(bundle: impl bevy_ecs::bundle::Bundle) -> mineflayer_engine::Client {
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
    let root = CancellationToken::new();
    let handle = dynamic_watcher(
        client.clone(),
        goal,
        Movements::default(),
        Arc::new(AtomicBool::new(false)),
        root.clone(),
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

    root.cancel();
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
    assert!(with_component::<PathfinderFacade, _>(&client, |f| f.goal.is_some()).unwrap_or(false));
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
