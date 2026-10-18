//! `Bot` — the connected bot handle, the Rust analog of the object mineflayer's
//! `createBot()` returns.
//!
//! [`Bot::builder`] / [`BotBuilder::connect`] are the `createBot(options)`
//! analog. Events are delivered over a [`tokio::sync::broadcast`] channel
//! (the multi-subscriber, lag-tolerant EventEmitter analog): call
//! [`Bot::events`] to subscribe.

use std::{sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

use mineflayer_engine::{
    core::{position::ChunkPos, registry_holder::RegistryHolder},
    ecs::component::{Component, Mutable},
    world::WorldName,
};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{broadcast, watch};

use crate::{Error, Result, chat::ChatShared};

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
    pub(crate) bootstrap_events: Arc<Mutex<Option<broadcast::Receiver<BotEvent>>>>,
    /// Dropping the last sender signals the bridge task to shut the client
    /// down (see `bridge::spawn`). `None` on driver handles: a background
    /// driver must not keep the connection open after the user drops every
    /// handle. Held only to keep the watch channel open.
    pub(crate) _alive: Option<watch::Sender<()>>,
    /// Cancelled by the bridge when it exits (disconnect, or every `Bot`
    /// handle gone). Every operation token is a child of it
    /// ([`crate::op::OpSlot`]), and driver tasks stop on it through
    /// [`TickStream`].
    pub(crate) root: CancellationToken,
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
///    or when it must be read in the same ECS access as the engine
///    components (`interact::InteractShared` with `Mining` and the start
///    message, `misc::MiscState` with `Pose`/`AirSupply`,
///    `pathfinder::PathfinderFacade` with the engine's `Pathfinder`).
/// 2. **ECS component, created on first use** for the plugin-analog state
///    (`tool`, `pvp`, `collect`, `auto_eat`): a plugin that is never used
///    allocates nothing, and the state goes away with the entity.
/// 3. **Here** for everything else: state that the bridge writes from
///    packets and `Bot` handles read, with no engine component involved
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
    /// Entity-domain state the ECS doesn't keep (equipment, raw attributes,
    /// vehicle links) — see `entity::EntityTracker`.
    pub entities: Arc<RwLock<crate::entity::EntityTracker>>,
    /// Inventory/window-domain state (current window, slot snapshots,
    /// eat/use bookkeeping) — see `windows::WindowTracker`.
    pub windows: Arc<Mutex<crate::windows::WindowTracker>>,
}

/// The game ticks a driver task loops on, until `token` is cancelled.
///
/// The token is what ends a driver: the `TickBroadcast` sender is a World
/// resource kept alive by the `Client` the driver holds, so the tick
/// channel never closes while the driver runs.
pub(crate) struct TickStream {
    ticks: broadcast::Receiver<()>,
    token: CancellationToken,
}

impl TickStream {
    pub(crate) fn new(client: &mineflayer_engine::Client, token: CancellationToken) -> Self {
        Self {
            ticks: client.get_tick_broadcaster(),
            token,
        }
    }

    /// Wait for the next tick. `false` once the token is cancelled or the
    /// tick channel closed. The channel holds one tick, so a slow driver
    /// loses ticks, not its loop.
    pub(crate) async fn next(&mut self) -> bool {
        tokio::select! {
            biased;
            _ = self.token.cancelled() => false,
            r = self.ticks.recv() => !matches!(r, Err(broadcast::error::RecvError::Closed)),
        }
    }
}

/// Storage rules 1 and 2: read a component on the client entity. `None`
/// when the entity or the component does not exist.
///
/// lock rule: `f` runs while the ECS lock is held, so it must not take a
/// domain lock.
pub(crate) fn with_component<T: Component, R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&T) -> R,
) -> Option<R> {
    let ecs = client.ecs.read();
    ecs.get_entity(client.entity).ok()?.get::<T>().map(f)
}

/// Storage rule 2: change a component on the client entity, created with
/// its default on first use. `None` when the entity does not exist.
///
/// lock rule: as for [`with_component`].
pub(crate) fn with_component_mut<T: Component<Mutability = Mutable> + Default, R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    let mut component = entity.entry::<T>().or_default().into_mut();
    Some(f(&mut component))
}

/// Lock rule (see docs/history/PORT-PLAN.md, B9). Two lock families meet here:
/// the domain locks in [`BotShared`] and the engine's ECS lock
/// (`client.ecs.read()/write()`, `get_component`, `entity_mut`,
/// `client.world()`).
///
/// 1. A domain lock **may** be held while the ECS lock is taken. The
///    window observer (ECS reads only) and the world-state observer do
///    this, and it is safe because the ECS guard is only ever held for a
///    moment under them.
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
            entities: Arc::new(RwLock::new(crate::entity::EntityTracker::default())),
            windows: Arc::new(Mutex::new(crate::windows::WindowTracker::new(
                events.clone(),
            ))),
            events,
        }
    }
}

mod connection;
mod event;
pub(crate) mod wait;
pub use connection::{Auth, BotBuilder};
pub use event::BotEvent;
pub use wait::WaitEnd;

/// `BotEvent` broadcast ring size, shared by every `Bot`. A busy world
/// emits several thousand events a second (one `Packet` per clientbound
/// packet plus per-entity domain events), so 256 overflowed mid-burst
/// and lagged subscribers silently missed events. 8192 is about one
/// second at the measured peak (8,751 events/s, `docs/performance.md`)
/// without the multi-MB upfront ring a much larger capacity would
/// allocate per `Bot`; `BotBuilder::packet_events(false)` removes the
/// largest share instead.
pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 8192;
impl Bot {
    /// `createBot(options)` — the builder entry point.
    pub fn builder() -> BotBuilder {
        BotBuilder::default()
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

    /// Every event as a [`Stream`](futures::Stream), from the same
    /// receiver [`Bot::events`] would hand out. A receiver that falls
    /// behind loses events; the stream then yields
    /// [`BotEvent::Lagged`] with the count, so the loss is visible. The
    /// stream ends when the bot's event channel closes.
    ///
    /// ```no_run
    /// # async fn demo(bot: mineflayer::Bot) {
    /// use futures::StreamExt;
    /// use mineflayer::BotEvent;
    ///
    /// let mut events = bot.event_stream();
    /// while let Some(event) = events.next().await {
    ///     match event {
    ///         BotEvent::Spawn => println!("spawned"),
    ///         BotEvent::Lagged(n) => eprintln!("lost {n} events"),
    ///         BotEvent::End(_) => break,
    ///         _ => {}
    ///     }
    /// }
    /// # }
    /// ```
    pub fn event_stream(&self) -> impl futures::Stream<Item = BotEvent> + Send + Unpin + 'static {
        Box::pin(futures::stream::unfold(
            self.events(),
            |mut rx| async move {
                match rx.recv().await {
                    Ok(event) => Some((event, rx)),
                    Err(broadcast::error::RecvError::Lagged(n)) => Some((BotEvent::Lagged(n), rx)),
                    Err(broadcast::error::RecvError::Closed) => None,
                }
            },
        ))
    }

    /// Wait for the first event that `pred` accepts: upstream's
    /// `once(bot, name)`, with a time limit so a lost packet cannot hang
    /// the caller. The wait also ends on [`BotEvent::End`], after `pred`
    /// has seen it.
    ///
    /// The wait subscribes when this method is called, not when the future
    /// is first polled. Call it before the action whose event it waits
    /// for. Like [`Bot::events`], the first wait after `connect` also sees
    /// the events from before `connect` returned.
    ///
    /// ```no_run
    /// # async fn demo(bot: mineflayer::Bot) -> mineflayer::Result<()> {
    /// use std::time::Duration;
    ///
    /// use mineflayer::BotEvent;
    ///
    /// // await once(bot, 'spawn')
    /// bot.wait_for(Duration::from_secs(30), |e| matches!(e, BotEvent::Spawn))
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for<F>(
        &self,
        timeout: Duration,
        pred: F,
    ) -> impl Future<Output = std::result::Result<BotEvent, WaitEnd>> + use<F>
    where
        F: FnMut(&BotEvent) -> bool,
    {
        let mut rx = self.events();
        async move { wait::await_event(&mut rx, timeout, pred).await }
    }

    /// [`Bot::wait_for`] for a wait that computes its result from the
    /// matching event: it ends on the first event that `f` maps to `Some`.
    ///
    /// ```no_run
    /// # async fn demo(bot: mineflayer::Bot) -> mineflayer::Result<()> {
    /// use std::time::Duration;
    ///
    /// use mineflayer::BotEvent;
    ///
    /// let reason = bot
    ///     .wait_map(Duration::from_secs(60), |e| match e {
    ///         BotEvent::Kicked { reason, .. } => Some(reason.clone()),
    ///         _ => None,
    ///     })
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_map<T, F>(
        &self,
        timeout: Duration,
        f: F,
    ) -> impl Future<Output = std::result::Result<T, WaitEnd>> + use<T, F>
    where
        F: FnMut(&BotEvent) -> Option<T>,
    {
        let mut rx = self.events();
        async move { wait::await_map(&mut rx, timeout, f).await }
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
    /// driver still stops when the root token is cancelled.
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
        let mut rx = self.subscribe();
        if self.spawn_chunks_loaded() {
            return Ok(());
        }
        // any event may have changed the world — re-check on each one
        match wait::await_event(&mut rx, TIMEOUT, |_| self.spawn_chunks_loaded()).await {
            Ok(_) => Ok(()),
            Err(wait::WaitEnd::Timeout) => Err(Error::Timeout),
            Err(_) if self.spawn_chunks_loaded() => Ok(()),
            Err(end) => Err(end.into()),
        }
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

    /// Lock policy: a panic while a domain lock is held must not break
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
        tx.send(BotEvent::Kicked {
            reason: "bye".to_string(),
            logged_in: true,
        })
        .unwrap();

        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Kicked { .. }));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Kicked { .. }));
    }

    /// A background driver must not keep the connection open: its handle
    /// drops the alive sender, so the bridge's alive channel closes when
    /// the last user handle drops, also while a driver runs. The control
    /// case (a plain clone, what drivers used to hold) keeps it open.
    #[tokio::test]
    async fn driver_handles_do_not_hold_the_alive_sender() {
        fn bot_with_alive(alive: watch::Sender<()>) -> Bot {
            let (events, _) = broadcast::channel(16);
            Bot {
                client: crate::test_util::despawned_client(),
                bootstrap_events: Arc::new(Mutex::new(None)),
                _alive: Some(alive),
                root: CancellationToken::new(),
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

    /// A receiver that falls behind yields `Lagged(n)` with the number of
    /// lost events, then the events that are still on the ring.
    #[tokio::test]
    async fn the_event_stream_reports_lost_events() {
        use futures::StreamExt;

        let bot = crate::test_util::bot(crate::test_util::despawned_client());
        let mut stream = bot.event_stream();
        let extra = 5;
        for _ in 0..EVENT_CHANNEL_CAPACITY + extra {
            bot.emit_event(BotEvent::Tick);
        }
        bot.emit_event(BotEvent::Spawn);
        assert!(matches!(
            stream.next().await,
            Some(BotEvent::Lagged(n)) if n == extra as u64 + 1
        ));
        assert!(matches!(stream.next().await, Some(BotEvent::Tick)));
    }

    /// `wait_for` subscribes when called, so an event sent before the
    /// first poll still ends the wait, and the first wait after `connect`
    /// sees the queued pre-connect events.
    #[tokio::test]
    async fn the_public_wait_subscribes_when_called() {
        let bot = crate::test_util::bot(crate::test_util::despawned_client());
        bot.emit_event(BotEvent::Spawn);
        let spawn = bot.wait_for(Duration::from_secs(1), |e| matches!(e, BotEvent::Spawn));
        assert!(matches!(spawn.await, Ok(BotEvent::Spawn)));

        let health = bot.wait_map(Duration::from_secs(1), |e| {
            matches!(e, BotEvent::Health).then_some(7)
        });
        bot.emit_event(BotEvent::Tick);
        bot.emit_event(BotEvent::Health);
        assert_eq!(health.await, Ok(7));

        let never = bot.wait_for(Duration::from_millis(10), |_| false);
        assert_eq!(never.await.unwrap_err(), WaitEnd::Timeout);
    }

    /// A crate-internal wait must not consume the bootstrap receiver; the
    /// first public subscription still sees the queued `Init`.
    #[tokio::test]
    async fn crate_waits_do_not_take_the_bootstrap_receiver() {
        let bot = crate::test_util::bot(crate::test_util::despawned_client());
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
        let bot = crate::test_util::bot(crate::test_util::despawned_client());
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
