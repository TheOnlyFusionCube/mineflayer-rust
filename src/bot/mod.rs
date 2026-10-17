//! `Bot` — the connected bot handle, the Rust analog of the object mineflayer's
//! `createBot()` returns.
//!
//! [`Bot::builder`] / [`BotBuilder::connect`] are the `createBot(options)`
//! analog. Events are delivered over a [`tokio::sync::broadcast`] channel
//! (the multi-subscriber, lag-tolerant EventEmitter analog): call
//! [`Bot::events`] to subscribe.

use std::{sync::Arc, time::Duration};

use mineflayer_engine::{
    core::{position::ChunkPos, registry_holder::RegistryHolder},
    world::WorldName,
};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{broadcast, watch};

use crate::{Error, Result, chat::ChatShared, world::WorldState};

/// A connected bot — the Rust analog of the object returned by mineflayer's
/// `createBot()`.
///
/// `Bot` is a cheap cloneable handle wrapping [`mineflayer_engine::Client`]. High-level
/// mineflayer methods (`bot.chat`, `bot.dig`, `bot.pathfinder`, …) are added
/// to it as `impl Bot` blocks in the domain modules.
///
/// When the last `Bot` handle is dropped, the client is disconnected and the
/// underlying ECS runner is shut down.
#[derive(Clone)]
pub struct Bot {
    pub(crate) client: mineflayer_engine::Client,
    /// The receiver the first [`Bot::events`] call hands out — subscribed
    /// before the bridge task started, so events that fired before
    /// `connect` returned (`Init`, `Login`, the spawn burst) are still on
    /// its ring. `None` once claimed.
    bootstrap_events: Arc<Mutex<Option<broadcast::Receiver<BotEvent>>>>,
    /// Dropping the last sender signals the bridge task to shut the client
    /// down (see `bridge::spawn`). `None` on driver handles: a background
    /// driver must not keep the connection open after the user drops every
    /// handle. Held only to keep the watch channel open.
    _alive: Option<watch::Sender<()>>,
    /// The bridge's shutdown signal: its sender lives in the bridge task
    /// and drops when the bridge exits (disconnect, or every `Bot`
    /// handle gone), resolving `changed()` for every receiver. The
    /// tick-driven driver tasks (`pathfinder` follow/dynamic watchers,
    /// `pvp::drive`) select on it — their `TickBroadcast` receiver can
    /// never report `Closed` because the sender is a World resource kept
    /// alive by the `Client` handle the driver itself holds.
    pub(crate) shutdown: watch::Receiver<()>,
    /// Scaffold bots ([`Bot::new`]) have no bridge to own the shutdown
    /// sender; keeping it here leaves `shutdown` open — a wrapped client
    /// is never "ended" from our side, matching pre-signal behavior.
    _shutdown_tx: Option<watch::Sender<()>>,
    /// Per-domain state shared with the bridge task; `Bot` method blocks
    /// reach domains through `self.shared` (see [`BotShared`]).
    pub(crate) shared: Arc<BotShared>,
    /// `options.chatLengthLimit` — the split limit `chat`/`whisper` chunk
    /// outgoing messages to (set once at build; upstream has no setter).
    pub(crate) chat_length_limit: usize,
}

/// The per-domain state every [`Bot`] handle and the bridge task share.
///
/// One construction site: a new domain is added here and in
/// [`BotShared::new`], instead of being threaded through `Bot`'s fields,
/// `connect_engine`, `Bot::new`, and the bridge's dependency bag. Each
/// domain keeps its own lock; nothing here is a single global lock.
///
/// Storage rule — where new per-bot state goes:
///
/// 1. **ECS component on the client entity** when an ECS system reads it
///    (`interact::InteractShared`, read by the dig-abort plugin) or when
///    it must be read in the same ECS access as the engine components
///    (`misc::MiscState` with `Pose`/`AirSupply`,
///    `pathfinder::PathfinderFacade` with the engine's `Pathfinder`).
/// 2. **ECS component, created on first use** for the plugin-analog state
///    (`tool`, `pvp`, `collect`, `auto_eat`): a plugin that is never used
///    allocates nothing, and the state goes away with the entity.
/// 3. **Here** for everything else: state that the bridge writes from
///    packets and `Bot` handles read, with no the engine component involved
///    (chat, world, entities, windows, typed windows, recipes,
///    scoreboard). A domain lock here never needs the ECS lock to read.
#[derive(Clone)]
pub(crate) struct BotShared {
    /// The bot's event stream (see [`Bot::events`]).
    pub events: broadcast::Sender<BotEvent>,
    /// Reason passed to [`Bot::quit`]/[`Bot::end`]; the bridge consumes it
    /// when the engine reports the disconnect so `BotEvent::End` can carry it
    /// (upstream: `client.end(reason)` -> `'end'` event).
    pub quit_reason: Arc<Mutex<Option<String>>>,
    /// The reason the last `BotEvent::End` carried — upstream chat.js's
    /// `endReason` (`_client.once('end', ...)`), read by `chat`/`whisper`
    /// to distinguish "never entered play" from "disconnected pre-play".
    pub end_reason: Arc<Mutex<Option<String>>>,
    /// Chat-domain state (pattern registry, tablist header/footer,
    /// `tab_complete` correlation) — see `chat::ChatShared`.
    pub chat: Arc<Mutex<ChatShared>>,
    /// Facade-kept world/game state (spawn point, clocks, weather, login
    /// metadata) fed by [`crate::world::observe_event`] inside the bridge
    /// task — the engine drops most of these packets upstream.
    pub world_state: Arc<RwLock<WorldState>>,
    /// Entity-domain state the ECS doesn't keep (equipment, raw attributes,
    /// vehicle links) — see `entity::EntityTracker`.
    pub entities: Arc<RwLock<crate::entity::EntityTracker>>,
    /// Inventory/window-domain state (current window, slot snapshots,
    /// eat/use bookkeeping) — see `windows::WindowTracker`.
    pub windows: Arc<Mutex<crate::windows::WindowTracker>>,
    /// Typed-window server state (container data, merchant offers,
    /// readiness) — see `window_types::WindowDataTracker`.
    pub window_data: Arc<Mutex<crate::window_types::WindowDataTracker>>,
    /// Recipe-book state (unlocked recipes from `c_recipe_book_add`/
    /// `c_recipe_book_remove`) — see `craft::RecipeBook`.
    pub recipes: Arc<Mutex<crate::craft::RecipeBook>>,
    /// Scoreboard/team/boss-bar state — see `scoreboard::ScoreboardState`.
    pub scoreboard: Arc<Mutex<crate::scoreboard::ScoreboardState>>,
}

/// Lock rule (see docs/history/PORT-PLAN.md, B9). Two lock families meet here:
/// the domain locks in [`BotShared`] and the engine's ECS lock
/// (`client.ecs.read()/write()`, `get_component`, `entity_mut`,
/// `client.world()`).
///
/// 1. A domain lock **may** be held while the ECS lock is taken. The
///    window observer and the world-state observer do this, and it is
///    safe because the ECS guard is only ever held for a moment under
///    them.
/// 2. Never take a domain lock while an ECS guard is alive — the
///    reverse order would cross with (1) and deadlock a queued writer.
///    If a value from the ECS is needed under a domain lock later, copy
///    it out, drop the guard, then lock.
impl BotShared {
    pub(crate) fn new(events: broadcast::Sender<BotEvent>) -> Self {
        Self {
            quit_reason: Arc::new(Mutex::new(None)),
            end_reason: Arc::new(Mutex::new(None)),
            chat: Arc::new(Mutex::new(ChatShared::default())),
            world_state: Arc::new(RwLock::new(WorldState::default())),
            entities: Arc::new(RwLock::new(crate::entity::EntityTracker::default())),
            windows: Arc::new(Mutex::new(crate::windows::WindowTracker::new(
                events.clone(),
            ))),
            window_data: Arc::new(Mutex::new(crate::window_types::WindowDataTracker::new())),
            recipes: Arc::new(Mutex::new(crate::craft::RecipeBook::default())),
            scoreboard: Arc::new(Mutex::new(crate::scoreboard::ScoreboardState::default())),
            events,
        }
    }
}

mod connection;
mod event;
pub(crate) mod wait;
pub use connection::{Auth, BotBuilder};
pub use event::BotEvent;

/// `BotEvent` broadcast ring size, shared by every `Bot`. A busy world
/// emits several thousand events a second (one `Packet` per clientbound
/// packet plus per-entity domain events), so 256 overflowed mid-burst
/// and lagged subscribers silently missed events. 8192 is about one
/// second at the measured peak (8,751 events/s, `docs/performance.md`)
/// without the multi-MB upfront ring a much larger capacity would
/// allocate per `Bot`; `BotBuilder::packet_events(false)` removes the
/// largest share instead.
const EVENT_CHANNEL_CAPACITY: usize = 8192;
impl Bot {
    /// `createBot(options)` — the builder entry point.
    pub fn builder() -> BotBuilder {
        BotBuilder::default()
    }

    /// Wrap an already-connected [`mineflayer_engine::Client`] in a `Bot` handle.
    ///
    /// Scaffolding: the resulting bot has no event bridge (no the engine event
    /// receiver exists for prebuilt clients), so [`Bot::events`] yields a
    /// silent stream. Prefer [`Bot::builder`].
    #[doc(hidden)]
    pub fn new(client: mineflayer_engine::Client) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (alive, _) = watch::channel(());
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        Self {
            client,
            bootstrap_events: Arc::new(Mutex::new(Some(events.subscribe()))),
            _alive: Some(alive),
            shutdown: shutdown_rx,
            _shutdown_tx: Some(shutdown_tx),
            shared: Arc::new(BotShared::new(events)),
            chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
        }
    }

    /// The underlying [`mineflayer_engine::Client`] — the `bot._client` analog for when
    /// the facade doesn't cover something yet.
    pub fn client(&self) -> &mineflayer_engine::Client {
        &self.client
    }

    /// Subscribe to this bot's event stream.
    ///
    /// The first call returns a receiver that was subscribed before the
    /// event bridge started — upstream's `createBot` lets listeners attach
    /// before anything fires, and this preserves that for the earliest
    /// events (`Init` is already queued in the engine's channel when `connect`
    /// returns, so a plain subscribe would race and lose it). Later calls
    /// see only events sent after they subscribe.
    ///
    /// Each call returns an independent receiver; slow consumers see
    /// [`broadcast::error::RecvError::Lagged`] rather than blocking the bot.
    ///
    /// Capacity and loss: the ring holds 8192 events for every receiver.
    /// A receiver that falls more than 8192 events behind gets `Lagged(n)`
    /// and the `n` oldest events are gone for it; other receivers are not
    /// affected. On the local fixture the stream measured about 1,400
    /// events/s idle and 3,300 events/s mean with a peak of 8,751 events/s
    /// near 200 chickens (`docs/performance.md`), so a receiver must not
    /// stall for about one second at peak. Raw [`BotEvent::Packet`] events
    /// are about 60% of the volume;
    /// [`BotBuilder::packet_events(false)`](crate::BotBuilder::packet_events)
    /// turns them off.
    ///
    /// Crate authors: inside the crate, use the private `subscribe` for
    /// waits. This method is the public API, and the first call hands out
    /// the bootstrap receiver.
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) {
    /// let mut events = bot.events();
    /// while let Ok(event) = events.recv().await {
    ///     match event {
    ///         BotEvent::Spawn => println!("spawned"),
    ///         BotEvent::End(reason) => {
    ///             println!("ended: {reason}");
    ///             break;
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// # }
    /// ```
    pub fn events(&self) -> broadcast::Receiver<BotEvent> {
        if let Some(rx) = self.bootstrap_events.lock().take() {
            return rx;
        }
        self.shared.events.subscribe()
    }

    /// Crate-internal receiver for waits inside the crate.
    ///
    /// Use this for every wait that the crate itself runs. It never takes
    /// the bootstrap receiver, so the first public [`Bot::events`] call
    /// still receives the pre-`connect` events (`Init`, `Login`, `Spawn`),
    /// and the crate's waits never complete on events from that queue.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<BotEvent> {
        self.shared.events.subscribe()
    }

    /// A handle for background driver tasks (auto-eat, pvp, pathfinder
    /// follow): it holds no `_alive` sender, so a running driver can never
    /// keep the connection open after the last user handle drops. The
    /// driver still stops on [`Bot::shutdown`] like every other task.
    pub(crate) fn driver_handle(&self) -> Bot {
        let mut bot = self.clone();
        bot._alive = None;
        bot
    }

    /// `bot.quit(reason?)` — graceful disconnect. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `disconnect.quitting`).
    pub fn quit(&self, reason: Option<&str>) {
        *self.shared.quit_reason.lock() = Some(reason.unwrap_or("disconnect.quitting").to_string());
        self.client.disconnect();
    }

    /// `bot.end(reason?)` — end the connection. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `socketClosed`).
    pub fn end(&self, reason: Option<&str>) {
        *self.shared.quit_reason.lock() = reason.map(str::to_string);
        self.client.disconnect();
    }

    /// `bot.respawn()` — manual respawn; a no-op while alive (upstream
    /// semantics). The engine auto-respawns on death (like mineflayer's default
    /// `respawn: true`), so this mainly exists for parity.
    pub fn respawn(&self) {
        if !self.is_alive() {
            use mineflayer_engine::protocol::packets::game::s_client_command::{
                Action, ServerboundClientCommand,
            };
            self.client.write_packet(ServerboundClientCommand {
                action: Action::PerformRespawn,
            });
        }
    }

    /// `bot.username` — the (possibly server-adjusted) account name; empty
    /// before the game profile arrives.
    pub fn username(&self) -> String {
        self.client
            .get_component::<mineflayer_engine::player::GameProfileComponent>()
            .map(|profile| profile.name.clone())
            .unwrap_or_default()
    }

    /// `bot.version` — the Minecraft version the engine targets (single-version
    /// port; see api-parity.md).
    pub fn version(&self) -> &'static str {
        mineflayer_engine::protocol::packets::VERSION_NAME
    }

    /// `bot.protocolVersion`.
    pub fn protocol_version(&self) -> i32 {
        mineflayer_engine::protocol::packets::PROTOCOL_VERSION
    }

    /// `bot.majorVersion` — single-version port: same as [`Bot::version`].
    pub fn major_version(&self) -> &'static str {
        mineflayer_engine::protocol::packets::VERSION_NAME
    }

    /// `bot.registry` — a snapshot of the server's dynamic registries
    /// (dimensions, enchantments, damage kinds, …). Static data
    /// (blocks/items/entities) lives in `mineflayer_engine::registry` / `mineflayer_engine::block`.
    pub fn registry(&self) -> RegistryHolder {
        self.client.with_registry_holder(|r| r.clone())
    }

    /// `bot.waitForTicks(n)` — resolves after `n` game ticks. Times out after
    /// `n * 50ms + 5s` like upstream (ticks stop when the world unloads).
    pub async fn wait_for_ticks(&self, ticks: u32) -> Result<()> {
        let limit = Duration::from_millis(u64::from(ticks) * 50 + 5000);
        tokio::time::timeout(limit, self.client.wait_ticks(ticks as usize))
            .await
            .map_err(|_| Error::Timeout)
    }

    /// `bot.waitForChunksToLoad()` — resolves once every chunk in the 5×5
    /// area centered on the bot's position is present in the client-loaded
    /// slice of the world (the engine's `PartialWorld`). 10s timeout, same as
    /// upstream.
    pub async fn wait_for_chunks_to_load(&self) -> Result<()> {
        const TIMEOUT: Duration = Duration::from_secs(10);
        tokio::time::timeout(TIMEOUT, async {
            let mut rx = self.subscribe();
            loop {
                if self.spawn_chunks_loaded() {
                    return Ok(());
                }
                match rx.recv().await {
                    // any event may have changed the world — just re-check
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return if self.spawn_chunks_loaded() {
                            Ok(())
                        } else {
                            Err(Error::NotConnected)
                        };
                    }
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// The `waitForChunksToLoad` condition: the bot entity is in the world
    /// and all 25 chunks of the 5×5 area centered on it are loaded.
    fn spawn_chunks_loaded(&self) -> bool {
        if !self.client.exists() {
            return false;
        }
        let pos = self.position();
        // `>> 4` matches upstream's int32 shift semantics
        let (cx, cz) = ((pos.x as i32) >> 4, (pos.z as i32) >> 4);
        let world = self.client.partial_world();
        let world = world.read();
        for dx in -2..=2 {
            for dz in -2..=2 {
                if world
                    .chunks
                    .limited_get(&ChunkPos::new(cx + dx, cz + dz))
                    .is_none()
                {
                    return false;
                }
            }
        }
        true
    }

    /// Emit a [`BotEvent`] to all subscribers — used by domain impls for
    /// upstream's `bot.emit('error', ...)` semantics inside sync methods.
    pub(crate) fn emit_event(&self, event: BotEvent) {
        let _ = self.shared.events.send(event);
    }

    /// Whether we've received the login packet and are in the game state.
    /// Unlike `mineflayer_engine::Client::logged_in` this never panics — it returns
    /// `false` once the entity is gone.
    pub(crate) fn logged_in(&self) -> bool {
        self.client
            .try_query_self::<Option<&WorldName>, _>(|w| w.is_some())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock policy (B8): a panic while a domain lock is held must not break
    /// the domain for later calls. Every crate lock is `parking_lot`, which
    /// does not poison; `std::sync` would panic on the second call.
    #[test]
    fn domain_lock_survives_a_panicking_observer() {
        let (events, _) = broadcast::channel(8);
        let shared = BotShared::new(events);
        let chat = shared.chat.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = chat.lock();
            panic!("an observer panicked while holding the chat lock");
        }));
        assert!(panicked.is_err(), "the observer panic must unwind");
        // the domain still works
        chat.lock().patterns.add(
            "probe",
            vec![regex::Regex::new("probe").unwrap()],
            crate::ChatPatternOpts::default(),
        );
        let events = chat
            .lock()
            .message_events(&mineflayer_engine::client_chat::ChatPacket::new("probe"));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, BotEvent::ChatPattern { name, .. } if name == "probe")),
            "the chat domain stopped working after the panic: {events:?}"
        );
    }

    #[test]
    fn bot_event_broadcast_reaches_all_subscribers() {
        let (tx, mut rx1) = broadcast::channel::<BotEvent>(16);
        let mut rx2 = tx.subscribe();

        tx.send(BotEvent::Login).unwrap();
        tx.send(BotEvent::Kicked("bye".to_string(), true)).unwrap();

        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Kicked(_, _)));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Kicked(_, _)));
    }

    /// A background driver must not keep the connection open: its handle
    /// drops the alive sender, so the bridge's alive channel closes when
    /// the last user handle drops, also while a driver runs. The control
    /// case (a plain clone, what drivers used to hold) keeps it open.
    #[tokio::test]
    async fn driver_handles_do_not_hold_the_alive_sender() {
        fn bot_with_alive(alive: watch::Sender<()>) -> Bot {
            let (events, _) = broadcast::channel(16);
            let (_shutdown_tx, shutdown_rx) = watch::channel(());
            // keep the shutdown sender alive for the test's lifetime
            std::mem::forget(_shutdown_tx);
            Bot {
                client: crate::test_util::despawned_client(),
                bootstrap_events: Arc::new(Mutex::new(None)),
                _alive: Some(alive),
                shutdown: shutdown_rx,
                _shutdown_tx: None,
                shared: Arc::new(BotShared::new(events)),
                chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
            }
        }

        // control: a plain clone in a task keeps the channel open
        let (alive_tx, mut alive_rx) = watch::channel(());
        let control = bot_with_alive(alive_tx);
        let held = control.clone();
        let _task = tokio::spawn(async move {
            let _held = held;
            std::future::pending::<()>().await;
        });
        drop(control);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), alive_rx.changed())
                .await
                .is_err(),
            "a held clone must keep the alive channel open"
        );

        // the driver handle drops the sender: the channel closes once the
        // last user handle is gone
        let (alive_tx, mut alive_rx) = watch::channel(());
        let bot = bot_with_alive(alive_tx);
        let driver = bot.driver_handle();
        let _task = tokio::spawn(async move {
            let _driver = driver;
            std::future::pending::<()>().await;
        });
        drop(bot);
        tokio::time::timeout(Duration::from_millis(200), alive_rx.changed())
            .await
            .expect("alive channel stayed open with only a driver handle left")
            .expect_err("alive channel reported a value instead of closing");
    }

    /// A crate-internal wait must not consume the bootstrap receiver; the
    /// first public subscription still sees the queued `Init`.
    #[tokio::test]
    async fn crate_waits_do_not_take_the_bootstrap_receiver() {
        let bot = Bot::new(crate::test_util::despawned_client());
        bot.emit_event(BotEvent::Init);

        // a crate wait polls first; the outer timeout only cancels it
        let _ = tokio::time::timeout(
            Duration::from_millis(50),
            bot.await_message(["never-matches"]),
        )
        .await;

        let mut user = bot.events();
        assert!(
            matches!(user.try_recv(), Ok(BotEvent::Init)),
            "a crate-internal wait consumed the bootstrap receiver"
        );
    }

    #[test]
    fn first_events_call_gets_the_pre_bridge_subscription() {
        // The engine can queue Init before `connect` returns; the first
        // `bot.events()` receiver was subscribed before the bridge started,
        // so those earliest events are still on it. Later subscribers only
        // see events sent after they attach.
        let bot = Bot::new(crate::test_util::despawned_client());
        bot.shared.events.send(BotEvent::Init).unwrap();
        bot.shared.events.send(BotEvent::Spawn).unwrap();

        let mut first = bot.events();
        let mut second = bot.events();
        assert!(matches!(first.try_recv().unwrap(), BotEvent::Init));
        assert!(matches!(first.try_recv().unwrap(), BotEvent::Spawn));
        assert!(matches!(
            second.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        // a later send reaches both
        bot.shared.events.send(BotEvent::Tick).unwrap();
        assert!(matches!(first.try_recv().unwrap(), BotEvent::Tick));
        assert!(matches!(second.try_recv().unwrap(), BotEvent::Tick));
    }

    #[test]
    fn bot_event_payloads_exist() {
        // Proves the event variants the lifecycle bridge needs are real and
        // carry the planned payload types.
        let _ = BotEvent::Init;
        let _ = BotEvent::Spawn;
        let _ = BotEvent::Respawn;
        let _ = BotEvent::Death;
        let _ = BotEvent::End("socketClosed".to_string());
        let _ = BotEvent::Error("boom".to_string());
        let _ = BotEvent::Tick;
        let _ = BotEvent::ChunkColumnLoad(ChunkPos::new(0, 0));
        let _ = |p: mineflayer_engine::client_chat::ChatPacket| BotEvent::Chat(p);
        let _ = |p: Arc<mineflayer_engine::protocol::packets::game::ClientboundGamePacket>| {
            BotEvent::Packet(p)
        };
        let _ = |i: mineflayer_engine::player::PlayerInfo| BotEvent::PlayerJoined(i);
    }
}
