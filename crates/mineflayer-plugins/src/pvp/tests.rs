use mineflayer::{
    engine::{Vec3, registry::identifier::Identifier, world::WorldName},
    test_util::{despawned_client, live_entity_client, tick_client},
};

use super::*;

/// A bot whose client entity is alive and carries a `WorldName` —
/// enough for `Bot::logged_in` to pass. No systems run on this ECS,
/// so the spawned driver/follow tasks die immediately (no
/// `TickBroadcast` resource) — the synchronous state transitions are
/// what's under test here.
fn spawned_bot() -> Bot {
    mineflayer::test_util::bot(live_entity_client((
        WorldName(Identifier::from("minecraft:overworld")),
        Position::new(Vec3::new(0., 64., 0.)),
    )))
}

/// A bot whose ECS carries a `TickBroadcast` — the driver's
/// `get_tick_broadcaster()` parks instead of panicking (no systems
/// run, so ticks only arrive when the test sends them), matching how
/// it waits on a live connection.
fn tick_bot() -> Bot {
    mineflayer::test_util::bot(tick_client((
        WorldName(Identifier::from("minecraft:overworld")),
        Position::new(Vec3::new(0., 64., 0.)),
    )))
}

/// Spawn an attackable-looking entity (positioned, so the driver
/// would gate on range) and get its ref.
fn spawn_target(bot: &Bot, x: f64, z: f64) -> EntityRef {
    let entity = bot
        .client()
        .ecs
        .write()
        .spawn(Position::new(Vec3::new(x, 64., z)))
        .id();
    bot.client().entity_ref_for(entity)
}

/// Drain every pending event; report whether `pred` matched one.
fn saw(rx: &mut tokio::sync::broadcast::Receiver<PvpEvent>, pred: fn(&PvpEvent) -> bool) -> bool {
    let mut saw = false;
    while let Ok(event) = rx.try_recv() {
        saw |= pred(&event);
    }
    saw
}

/// Drain every pending event; report whether a `StoppedAttacking`
/// was among them.
fn saw_stopped_attacking(rx: &mut tokio::sync::broadcast::Receiver<PvpEvent>) -> bool {
    saw(rx, |e| matches!(e, PvpEvent::StoppedAttacking))
}

#[tokio::test]
async fn attack_then_stop_transitions_and_reports_once() {
    let bot = spawned_bot();
    let mut rx = bot.plugin_events::<PvpEvent>();
    let target = spawn_target(&bot, 5., 0.);

    // start → engaged: the loop records its target and reports it
    bot.pvp().attack(&target).unwrap();
    assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));
    assert!(saw(&mut rx, |e| matches!(e, PvpEvent::StartedAttacking)));

    // stop → idle: target cleared, the follow goal ours to remove,
    // and the stopped event emitted
    bot.pvp().stop();
    assert!(bot.pvp().target().is_none());
    assert!(bot.pathfinder().goal().is_none());
    assert!(saw_stopped_attacking(&mut rx));

    // stopping again is a silent no-op — no state, no event
    let mut rx = bot.plugin_events::<PvpEvent>();
    bot.pvp().stop();
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn retarget_swaps_without_reporting_a_stop() {
    let bot = spawned_bot();
    let mut rx = bot.plugin_events::<PvpEvent>();
    let first = spawn_target(&bot, 5., 0.);
    let second = spawn_target(&bot, -5., 0.);

    bot.pvp().attack(&first).unwrap();
    bot.pvp().attack(&second).unwrap();
    assert_eq!(bot.pvp().target().map(|t| t.id()), Some(second.id()));
    // a retarget never stopped the loop — no StoppedAttacking, but
    // upstream's startedAttacking fires per attack() call
    let mut started = 0;
    while let Ok(event) = rx.try_recv() {
        started += usize::from(matches!(event, PvpEvent::StartedAttacking));
        assert!(!matches!(event, PvpEvent::StoppedAttacking));
    }
    assert_eq!(started, 2);

    bot.pvp().stop();
}

/// upstream `PVP.ts`: `if (target === this.target) return;` — attacking
/// the already-active target is a silent no-op (no fresh goal, no
/// second `StartedAttacking`).
#[tokio::test]
async fn attack_same_target_is_a_no_op() {
    let bot = spawned_bot();
    let mut rx = bot.plugin_events::<PvpEvent>();
    let target = spawn_target(&bot, 5., 0.);

    bot.pvp().attack(&target).unwrap();
    bot.pvp().attack(&target).unwrap();
    assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));
    let mut started = 0;
    while let Ok(event) = rx.try_recv() {
        started += usize::from(matches!(event, PvpEvent::StartedAttacking));
    }
    assert_eq!(started, 1);

    bot.pvp().stop();
}

#[tokio::test]
async fn force_stop_clears_a_foreign_goal_unlike_stop() {
    let bot = spawned_bot();
    let mut rx = bot.plugin_events::<PvpEvent>();
    let target = spawn_target(&bot, 5., 0.);
    let other = spawn_target(&bot, -5., 0.);
    bot.pvp().attack(&target).unwrap();

    // a follow goal on a *different* entity installed after
    // attack() isn't ours — stop() leaves it running, force_stop()
    // clears it like upstream's `setGoal(null)`
    bot.pathfinder()
        .set_goal(
            Goal::Follow {
                entity: other.clone(),
                range: 1.,
            },
            true,
        )
        .unwrap();
    bot.pvp().stop();
    assert!(matches!(
        bot.pathfinder().goal(),
        Some(Goal::Follow { entity, .. }) if entity.id() == other.id()
    ));

    bot.pvp().attack(&target).unwrap();
    bot.pathfinder()
        .set_goal(
            Goal::Follow {
                entity: other.clone(),
                range: 1.,
            },
            true,
        )
        .unwrap();
    bot.pvp().force_stop();
    assert!(bot.pvp().target().is_none());
    assert!(bot.pathfinder().goal().is_none());
    assert!(saw_stopped_attacking(&mut rx));

    // idle force_stop is a no-op
    let mut rx = bot.plugin_events::<PvpEvent>();
    bot.pvp().force_stop();
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn tunables_round_trip_and_feed_the_follow_goal() {
    let bot = spawned_bot();
    // upstream defaults
    assert_eq!(bot.pvp().view_distance(), 128.);
    assert_eq!(bot.pvp().attack_range(), 3.);
    assert_eq!(bot.pvp().follow_range(), 2.);
    assert!(matches!(
        bot.pvp().melee_attack_rate(),
        AttackRate::CooldownOffset { min: -2, max: 2 }
    ));
    assert!(bot.pvp().movements().is_some());

    bot.pvp().set_view_distance(64.);
    bot.pvp().set_attack_range(2.5);
    bot.pvp().set_follow_range(1.5);
    bot.pvp()
        .set_melee_attack_rate(AttackRate::RandomTicks { min: 5, max: 8 });
    bot.pvp().set_movements(None);
    assert_eq!(bot.pvp().view_distance(), 64.);
    assert_eq!(bot.pvp().attack_range(), 2.5);
    assert_eq!(bot.pvp().follow_range(), 1.5);
    assert!(matches!(
        bot.pvp().melee_attack_rate(),
        AttackRate::RandomTicks { min: 5, max: 8 }
    ));
    assert!(bot.pvp().movements().is_none());

    // follow_range reaches the installed goal
    let target = spawn_target(&bot, 5., 0.);
    bot.pvp().attack(&target).unwrap();
    assert!(matches!(
        bot.pathfinder().goal(),
        Some(Goal::Follow { range, .. }) if range == 1.5
    ));
    bot.pvp().stop();
}

#[test]
fn attack_rate_samples_in_range_and_clamps() {
    for _ in 0..256 {
        let t = AttackRate::RandomTicks { min: 10, max: 20 }.ticks(0);
        assert!((10..20).contains(&t));
        // cooldown + offset, clamped ≥1 like upstream's getTicks
        let t = AttackRate::CooldownOffset { min: -2, max: 2 }.ticks(13);
        assert!((11..15).contains(&t));
    }
    // a negative offset can pull the sum below 1 — upstream clamps
    assert_eq!(AttackRate::CooldownOffset { min: -10, max: -5 }.ticks(3), 1);
    // empty ranges collapse to min
    assert_eq!(AttackRate::RandomTicks { min: 7, max: 7 }.ticks(0), 7);
}

/// A target beyond `view_distance` counts as defeated — the driver
/// ends the run the way `stop()` would (upstream `checkRange`).
#[tokio::test]
async fn target_past_view_distance_ends_the_loop_and_reports() {
    let bot = tick_bot();
    let mut rx = bot.plugin_events::<PvpEvent>();
    // well past the default 128 — no components for the driver to
    // attack, but Position is enough for the distance check
    let target = spawn_target(&bot, 200., 0.);
    let op = bot
        .state_mut::<PvpState, _>(|s| {
            s.target = Some(target.clone());
            bot.start_op(&mut s.op)
        })
        .unwrap();
    let sender = mineflayer::test_util::tick_sender(bot.client());
    let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));

    // a broadcast send lands only for receivers already subscribed —
    // let the driver park on its tick receiver first
    while sender.receiver_count() == 0 {
        tokio::task::yield_now().await;
    }
    sender.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), driver)
        .await
        .expect("driver kept chasing a defeated target")
        .unwrap();
    assert!(bot.pvp().target().is_none());
    assert!(saw_stopped_attacking(&mut rx));
}

/// A live `view_distance` retune takes effect on the very next tick
/// — upstream reads the field each physics tick.
#[tokio::test]
async fn view_distance_retune_ends_a_running_loop() {
    let bot = tick_bot();
    let target = spawn_target(&bot, 50., 0.);
    let op = bot
        .state_mut::<PvpState, _>(|s| {
            s.target = Some(target.clone());
            bot.start_op(&mut s.op)
        })
        .unwrap();
    let sender = mineflayer::test_util::tick_sender(bot.client());
    let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));

    // inside the default 128 the loop keeps running
    bot.pvp().set_view_distance(10.);
    // a broadcast send lands only for receivers already subscribed —
    // let the driver park on its tick receiver first
    while sender.receiver_count() == 0 {
        tokio::task::yield_now().await;
    }
    sender.send(()).unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), driver)
        .await
        .expect("driver ignored a live view_distance retune")
        .unwrap();
    assert!(bot.pvp().target().is_none());
}

#[tokio::test]
async fn attack_before_spawn_is_not_connected() {
    let client = despawned_client();
    let bot = mineflayer::test_util::bot(client.clone());
    let target = client.entity_ref_for(client.entity);
    assert!(matches!(
        bot.pvp().attack(&target),
        Err(Error::NotConnected)
    ));
    assert!(bot.pvp().target().is_none());
}

/// Disconnect retires the driver: the tick channel can't close on its
/// own (the driver's `Client` keeps the `TickBroadcast` sender alive),
/// so the root token, which the bridge cancels on exit, is the only
/// signal.
#[tokio::test]
async fn disconnect_ends_the_loop_and_reports() {
    let (bot, shutdown) = mineflayer::test_util::bot_with_shutdown(tick_client((
        WorldName(Identifier::from("minecraft:overworld")),
        Position::new(Vec3::new(0., 64., 0.)),
    )));
    let mut rx = bot.plugin_events::<PvpEvent>();
    let target = spawn_target(&bot, 5., 0.);
    let op = bot
        .state_mut::<PvpState, _>(|s| {
            s.target = Some(target.clone());
            bot.start_op(&mut s.op)
        })
        .unwrap();
    let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));
    // parked on ticks like a live connection — target still recorded
    assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));

    // the bridge's exit
    drop(shutdown);

    tokio::time::timeout(std::time::Duration::from_secs(5), driver)
        .await
        .expect("driver outlived the connection")
        .unwrap();
    assert!(bot.pvp().target().is_none());
    assert!(saw_stopped_attacking(&mut rx));
}
