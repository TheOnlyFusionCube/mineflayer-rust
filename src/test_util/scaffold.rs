//! Clients and bots with no engine systems, for unit tests.

use std::ops::Deref as _;

/// A `Client` whose own entity is already despawned — the state packets
/// still in flight during teardown race into.
pub(crate) fn despawned_client() -> mineflayer_engine::Client {
    let mut app = mineflayer_engine::app::App::new();
    // `Client::new` wants `Arc<RwLock<World>>`; `start_ecs_runner` hands
    // one back without naming the lock type.
    let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
    let entity = {
        let mut ecs = ecs.write();
        let entity = ecs.spawn_empty().id();
        ecs.despawn(entity);
        entity
    };
    mineflayer_engine::Client::new(entity, ecs)
}

/// A `Client` whose own entity is alive and carries `bundle` — for
/// tests that need components present (plugin state, pathfinder
/// facade, inventory). No systems run, so ECS events are queued but
/// never processed and tick broadcasters may not exist.
pub(crate) fn live_entity_client(
    bundle: impl mineflayer_engine::ecs::bundle::Bundle,
) -> mineflayer_engine::Client {
    let mut app = mineflayer_engine::app::App::new();
    let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
    let entity = ecs.write().spawn(bundle).id();
    mineflayer_engine::Client::new(entity, ecs)
}

/// A `Client` whose ECS carries a `TickBroadcast` — tick-driven waits
/// park on `get_tick_broadcaster()` instead of panicking. No systems
/// run, so ticks only arrive when the test sends them via
/// [`tick_sender`].
pub(crate) fn tick_client(
    bundle: impl mineflayer_engine::ecs::bundle::Bundle,
) -> mineflayer_engine::Client {
    let mut app = mineflayer_engine::app::App::new();
    app.add_plugins(mineflayer_engine::tick_broadcast::TickBroadcastPlugin);
    let (ecs, _run_systems, _exit_rx) = mineflayer_engine::start_ecs_runner(app.main_mut());
    let entity = ecs.write().spawn(bundle).id();
    mineflayer_engine::Client::new(entity, ecs)
}

/// The `TickBroadcast` sender — capacity 1, so two sends while a
/// receiver is parked make its next `recv` report `Lagged(1)`, the
/// routine scheduling stall the tick drivers must survive.
pub(crate) fn tick_sender(
    client: &mineflayer_engine::Client,
) -> tokio::sync::broadcast::Sender<()> {
    client
        .ecs
        .read()
        .resource::<mineflayer_engine::tick_broadcast::TickBroadcast>()
        .deref()
        .clone()
}

/// A `Bot` over a prebuilt `Client` — the unit-test scaffold: no event
/// bridge (so `Bot::events()` yields a silent stream), and a root token
/// that nothing cancels.
pub(crate) fn bot(client: mineflayer_engine::Client) -> crate::Bot {
    let (events, _) = tokio::sync::broadcast::channel(crate::bot::EVENT_CHANNEL_CAPACITY);
    let (alive, _) = tokio::sync::watch::channel(());
    crate::Bot {
        client,
        bootstrap_events: std::sync::Arc::new(parking_lot::Mutex::new(Some(events.subscribe()))),
        _alive: Some(alive),
        root: tokio_util::sync::CancellationToken::new(),
        shared: std::sync::Arc::new(crate::bot::BotShared::new(events)),
        chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
    }
}

/// A `Bot` and a guard over its root token: drop the guard to play the
/// bridge's exit (disconnect), which cancels the root.
pub(crate) fn bot_with_shutdown(
    client: mineflayer_engine::Client,
) -> (crate::Bot, tokio_util::sync::DropGuard) {
    let bot = bot(client);
    let guard = bot.root.clone().drop_guard();
    (bot, guard)
}
