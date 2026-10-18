use super::*;
use mineflayer_engine::pathfinder;

/// A client whose ECS carries the pathfinder message resources, so a
/// goto lands in `Messages<GotoEvent>` where the test can count it. No
/// systems run.
pub(super) fn pathfinder_client(
    bundle: impl bevy_ecs::bundle::Bundle,
) -> mineflayer_engine::Client {
    let mut app = mineflayer_engine::app::App::new();
    app.add_plugins(mineflayer_engine::tick_broadcast::TickBroadcastPlugin)
        .add_plugins(pathfinder::PathfinderPlugin);
    let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
    let entity = ecs.write().spawn(bundle).id();
    mineflayer_engine::Client::new(entity, ecs)
}

/// Count of `GotoEvent`s sent so far.
pub(super) fn goto_messages(client: &mineflayer_engine::Client) -> usize {
    client
        .ecs
        .read()
        .resource::<bevy_ecs::message::Messages<pathfinder::GotoEvent>>()
        .len()
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
/// future's first poll.
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
/// identical goal is not a replacement, so a pending `goto` survives it
/// (upstream's identity check wouldn't reject either).
#[tokio::test]
async fn goto_survives_identical_goal_reissue() {
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
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

    pf2.set_goal(mk(&client), false).unwrap();
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
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
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
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
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
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
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

/// `goto` rejects `NoPath` when the bridge sees its goto's search fail.
#[tokio::test]
async fn goto_rejects_no_path_from_its_search() {
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
    let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
    let pf = bot.pathfinder();

    let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
    await_install(&mut goto, &client).await;

    let id = with_component::<PathfinderFacade, _>(&client, |f| f.goto)
        .flatten()
        .expect("the goal runs as a goto");
    bridge_events(
        &client,
        &Event::Pathfinder(PathEvent::PathFound {
            goto: id,
            path: Vec::new(),
            is_partial: true,
            elapsed: Duration::from_millis(5),
        }),
    );
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
        .await
        .expect("goto hung on a failed search")
        .expect_err("the failed goto resolved");
    assert!(
        matches!(err, Error::Path(PathError::NoPath)),
        "expected NoPath, got {err:?}"
    );
}

/// The end of the connection rejects a pending `goto`.
#[tokio::test]
async fn goto_rejects_on_disconnect() {
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
    let (bot, shutdown) = crate::test_util::bot_with_shutdown(client.clone());
    let pf = bot.pathfinder();

    let mut goto = std::pin::pin!(pf.goto(follow_at(&client, Vec3::new(500., 64., 0.))));
    await_install(&mut goto, &client).await;
    drop(shutdown);
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), &mut goto)
        .await
        .expect("goto outlived the connection")
        .expect_err("a disconnected goto resolved");
    assert!(matches!(err, Error::NotConnected), "{err:?}");
}

/// `goto` resolves when the goal is already met — upstream's
/// `status === 'success' && path.length === 0` fast path (here: the
/// `goal_reached` `set_goal` emits when `is_end` already holds).
#[tokio::test]
async fn goto_resolves_on_already_satisfied_goal() {
    let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
    let (bot, _shutdown) = crate::test_util::bot_with_shutdown(client.clone());
    let pf = bot.pathfinder();
    // the follow target is already within range of the bot
    let goto = pf.goto(follow_at(&client, Vec3::new(0.5, 64., 0.)));
    tokio::time::timeout(std::time::Duration::from_secs(5), goto)
        .await
        .expect("goto hung on a satisfied goal")
        .expect("the satisfied goto rejected");
}
