//! `Bot` — the connected bot handle, the Rust analog of the object mineflayer's
//! `createBot()` returns.
//!
//! [`Bot::builder`] / [`BotBuilder::connect`] are the `createBot(options)`
//! analog. Events are delivered over a [`tokio::sync::broadcast`] channel
//! (the multi-subscriber, lag-tolerant EventEmitter analog): call
//! [`Bot::events`] to subscribe.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use azalea::{
    auto_reconnect::AutoReconnectDelay,
    core::{position::ChunkPos, registry_holder::RegistryHolder},
    player::PlayerInfo,
    protocol::packets::game::ClientboundGamePacket,
    world::WorldName,
};
use std::sync::RwLock;
use tokio::sync::{broadcast, watch};

use crate::world::{Block, WorldState};
use crate::{Error, Result, bridge};
use azalea::{EntityRef, protocol::packets::game::c_block_entity_data::ClientboundBlockEntityData};

/// A connected bot — the Rust analog of the object returned by mineflayer's
/// `createBot()`.
///
/// `Bot` is a cheap cloneable handle wrapping [`azalea::Client`]. High-level
/// mineflayer methods (`bot.chat`, `bot.dig`, `bot.pathfinder`, …) are added
/// to it as `impl Bot` blocks in the domain modules.
///
/// When the last `Bot` handle is dropped, the client is disconnected and the
/// underlying ECS runner is shut down.
#[derive(Clone)]
pub struct Bot {
    pub(crate) client: azalea::Client,
    events: broadcast::Sender<BotEvent>,
    /// Reason passed to [`Bot::quit`]/[`Bot::end`]; the bridge consumes it when
    /// azalea reports the disconnect so `BotEvent::End` can carry it
    /// (upstream: `client.end(reason)` -> `'end'` event).
    pub(crate) quit_reason: Arc<Mutex<Option<String>>>,
    /// Dropping the last sender signals the bridge task to shut the client
    /// down (see `bridge::spawn`). Held only to keep the watch channel open.
    _alive: watch::Sender<()>,
    /// Facade-kept world/game state (spawn point, clocks, weather, login
    /// metadata) maintained by the packet bridge in [`crate::world`] —
    /// azalea drops most of these packets upstream.
    pub(crate) world_state: Arc<RwLock<WorldState>>,
}

/// Events emitted by a [`Bot`], mirroring mineflayer's `bot.on(...)` events.
///
/// Bridged from azalea's [`Event`](azalea::Event) enum; variant payloads use
/// azalea's types so no data is lost in translation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BotEvent {
    /// The client just started, before login. No direct mineflayer analog
    /// (closest: `inject_allowed`); used internally to apply `view_distance`
    /// before the server sees our client information.
    Init,
    /// `login` — the server accepted our login.
    Login,
    /// `spawn` — the bot spawned into a loaded world.
    Spawn,
    /// `respawn` — emitted right before every `Spawn` after the first
    /// (respawn after death, dimension change), matching mineflayer's
    /// respawn-then-spawn ordering.
    Respawn,
    /// `death` — the bot died.
    Death,
    /// `kicked` — the server disconnected us with a reason (in-game kicks
    /// only; see api-parity.md for the login-phase caveat).
    Kicked(String),
    /// `end` — the connection ended, with the reason (`socketClosed`,
    /// the `quit`/`end` argument, or a kick reason).
    End(String),
    /// `error` — a non-fatal error was surfaced (currently: connection
    /// failures before login).
    Error(String),
    /// `chat` — a chat packet arrived. Richer `message`/`whisper`/pattern
    /// events land with the chat unit.
    Chat(azalea::chat::ChatPacket),
    /// `physicsTick` — a game tick elapsed while the world was loaded.
    Tick,
    /// Raw clientbound packet escape hatch for APIs azalea doesn't lift
    /// (scoreboards, boss bars, titles, particles, …). Emitted under azalea's
    /// `packet-event` feature, which is on by default.
    Packet(Arc<ClientboundGamePacket>),
    /// `playerJoined` — a player was added to the tab list.
    PlayerJoined(PlayerInfo),
    /// `playerUpdated` — a tab-list entry changed (gamemode, display name,
    /// latency).
    PlayerUpdated(PlayerInfo),
    /// `playerLeft` — a player was removed from the tab list.
    PlayerLeft(PlayerInfo),
    /// `chunkColumnLoad` — a chunk column was received.
    ChunkColumnLoad(ChunkPos),
    /// `chunkColumnUnload` — a chunk column was unloaded by the server.
    ChunkUnload(ChunkPos),
    /// `blockUpdate` — a block changed in the world. `old` is `None` when
    /// the previous state wasn't observed; `new` is what the server
    /// reported. Azalea applies updates to the world itself on the next
    /// tick, so `block_at` may briefly still return `old`.
    ///
    /// mineflayer's position-filtered `blockUpdate:x,y,z` is spelled by
    /// matching `new.pos` on this variant.
    BlockUpdate { old: Option<Block>, new: Block },
    /// `blockEntityData` — raw block-entity NBT. Azalea doesn't store
    /// block entities, so the packet is forwarded as received.
    BlockEntityData(ClientboundBlockEntityData),
    /// `signOpen` — the server opened a sign editor (usually right after
    /// a sign was placed); answer with `Bot::update_sign`.
    SignOpen {
        block: Option<Block>,
        is_front_text: bool,
    },
    /// `pistonMove` — a piston extended (`is_pulling: false`) or
    /// retracted. `direction` is the raw protocol facing byte
    /// (0=down, 1=up, 2=north, 3=south, 4=west, 5=east).
    PistonMove {
        block: Block,
        is_pulling: bool,
        direction: u8,
    },
    /// `chestLidMove` — the open-count of a chest or other lidded
    /// container changed. `second` is the other half for double chests.
    ChestLidMove {
        block: Block,
        open_count: u8,
        second: Option<Block>,
    },
    /// `blockBreakProgressObserved` — a break-animation stage (0–9).
    BlockBreakProgress {
        block: Option<Block>,
        stage: u8,
        entity: Option<EntityRef>,
    },
    /// `blockBreakProgressEnd` — the break animation was removed.
    BlockBreakEnd {
        block: Option<Block>,
        entity: Option<EntityRef>,
    },
    /// `time` — a `set_time` packet updated the world clocks.
    Time,
    /// `rain` — `is_raining` flipped.
    Rain,
    /// `weatherUpdate` — the rain or thunder level changed.
    WeatherUpdate,
    /// `game` — game state changed (login/respawn metadata, gamemode,
    /// default spawn position).
    GameStateChanged,
}

/// `createBot(options)` analog. Defaults match upstream:
/// `localhost:25565`, username `Player`, offline auth.
///
/// ```no_run
/// # async fn example() -> mineflayer::Result<()> {
/// let bot = mineflayer::Bot::builder()
///     .host("localhost")
///     .port(25566)
///     .username("ferris")
///     .connect()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct BotBuilder {
    /// `options.host` (default `localhost`).
    host: String,
    /// `options.port` (default `25565`).
    port: u16,
    /// `options.username` (default `Player`). Used when `auth` isn't set.
    username: String,
    /// `options.auth` (default `Offline { username }`).
    auth: Option<Auth>,
    /// `options.viewDistance` — applied via `set_client_information` on
    /// `Event::Init`, so the login packet already carries it.
    view_distance: Option<u8>,
}

/// How the bot authenticates — covers `auth`, `accessToken`, and prebuilt
/// accounts. (No password auth: Mojang password login is dead upstream too;
/// azalea uses the Microsoft device-code flow or access tokens.)
#[derive(Clone)]
pub enum Auth {
    /// `auth: 'offline'` — no Mojang login; `username` is sent as-is.
    Offline {
        /// The username presented to the server.
        username: String,
    },
    /// `auth: 'microsoft'` — azalea's interactive device-code flow (prints a
    /// link code; caches tokens in `~/.minecraft/azalea-auth-cache.json`).
    Microsoft {
        /// Microsoft account email — the token cache key.
        email: String,
    },
    /// A prebuilt [`azalea::account::Account`] — e.g.
    /// `Account::with_microsoft_access_token(token)` (the `accessToken`
    /// option) or a custom `AccountTrait` impl.
    Account(azalea::account::Account),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Account` (Arc<dyn AccountTrait>) has no Debug; don't leak tokens.
        match self {
            Auth::Offline { username } => f
                .debug_struct("Offline")
                .field("username", username)
                .finish(),
            Auth::Microsoft { email } => f.debug_struct("Microsoft").field("email", email).finish(),
            Auth::Account(_) => f.write_str("Account(..)"),
        }
    }
}

impl Default for BotBuilder {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 25565,
            username: "Player".to_string(),
            auth: None,
            view_distance: None,
        }
    }
}

impl BotBuilder {
    /// `options.host`.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }
    /// `options.port`.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
    /// `options.username`.
    pub fn username(mut self, username: impl Into<String>) -> Self {
        self.username = username.into();
        self
    }
    /// `options.auth`. An explicit [`Auth::Offline`] wins over `username`.
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }
    /// `options.viewDistance`, in chunks.
    pub fn view_distance(mut self, view_distance: u8) -> Self {
        self.view_distance = Some(view_distance);
        self
    }

    /// `createBot` — resolve the address, start the client, spawn the event
    /// bridge, and return the bot handle.
    ///
    /// Like upstream, this returns before the bot is in the world: connection
    /// problems surface later as [`BotEvent::Error`] + [`BotEvent::End`] on
    /// [`Bot::events`] rather than from this call (only address-resolution
    /// failures are returned directly).
    ///
    /// The connection and ECS run on a dedicated thread — azalea's ECS runner
    /// uses `tokio::task::spawn_local`, which requires a `LocalSet` context —
    /// so `connect` works from any async context, including the default
    /// `#[tokio::main]`. The thread exits when the client does.
    pub async fn connect(self) -> Result<Bot> {
        let account = match self.auth {
            Some(Auth::Offline { username }) => azalea::account::Account::offline(&username),
            Some(Auth::Microsoft { email }) => azalea::account::Account::microsoft(&email)
                .await
                .map_err(Error::Auth)?,
            Some(Auth::Account(account)) => account,
            None => azalea::account::Account::offline(&self.username),
        };

        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let address = format!("{}:{}", self.host, self.port);
        let view_distance = self.view_distance;
        std::thread::Builder::new()
            .name(format!("mineflayer-bot-{}", self.username))
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("mineflayer: build bot runtime");
                rt.block_on(async {
                    let local = tokio::task::LocalSet::new();
                    local.spawn_local(async move {
                        let _ = init_tx
                            .send(Self::connect_azalea(account, address, view_distance).await);
                    });
                    // Drive the LocalSet until all local tasks finish. azalea's
                    // ECS runner is a local task; it exits when the client does
                    // (last `Bot` dropped → `client.exit()`, or fatal error).
                    local.await;
                });
            })
            .map_err(|_| Error::Connect("could not spawn the bot thread".into()))?;

        init_rx
            .await
            .map_err(|_| Error::Connect("bot thread exited during connect".into()))?
    }

    /// Join the server and wire up the event bridge. Runs inside the bot
    /// thread's `LocalSet` (azalea requires it for `Client::join`).
    async fn connect_azalea(
        account: azalea::account::Account,
        address: String,
        view_distance: Option<u8>,
    ) -> Result<Bot> {
        let (client, rx) = azalea::Client::join(account, address).await?;

        // azalea's default plugins auto-reconnect 5s after any disconnect —
        // including our own `disconnect()`. That would silently break
        // `end`/`quit`/`kicked` semantics, so opt out for this client
        // (Duration::MAX is azalea's documented "disabled" value).
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(AutoReconnectDelay::new(Duration::MAX));

        let (events, _) = broadcast::channel(256);
        let quit_reason = Arc::new(Mutex::new(None));
        let (alive, alive_rx) = watch::channel(());
        let world_state = Arc::new(RwLock::new(WorldState::default()));
        let world_alive = alive_rx.clone();
        bridge::spawn(
            client.clone(),
            rx,
            events.clone(),
            view_distance,
            quit_reason.clone(),
            alive_rx,
        );
        crate::world::spawn_packet_bridge(
            client.clone(),
            events.clone(),
            world_state.clone(),
            world_alive,
        );

        Ok(Bot {
            client,
            events,
            quit_reason,
            _alive: alive,
            world_state,
        })
    }
}

impl Bot {
    /// `createBot(options)` — the builder entry point.
    pub fn builder() -> BotBuilder {
        BotBuilder::default()
    }

    /// Wrap an already-connected [`azalea::Client`] in a `Bot` handle.
    ///
    /// Scaffolding: the resulting bot has no event bridge (no azalea event
    /// receiver exists for prebuilt clients), so [`Bot::events`] yields a
    /// silent stream. Prefer [`Bot::builder`].
    #[doc(hidden)]
    pub fn new(client: azalea::Client) -> Self {
        let (events, _) = broadcast::channel(256);
        let (alive, _) = watch::channel(());
        Self {
            client,
            events,
            quit_reason: Arc::new(Mutex::new(None)),
            _alive: alive,
            world_state: Arc::new(RwLock::new(WorldState::default())),
        }
    }

    /// The underlying [`azalea::Client`] — the `bot._client` analog for when
    /// the facade doesn't cover something yet.
    pub fn client(&self) -> &azalea::Client {
        &self.client
    }

    /// Subscribe to this bot's event stream.
    ///
    /// Each call returns an independent receiver; slow consumers see
    /// [`broadcast::error::RecvError::Lagged`] rather than blocking the bot.
    pub fn events(&self) -> broadcast::Receiver<BotEvent> {
        self.events.subscribe()
    }

    /// `bot.quit(reason?)` — graceful disconnect. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `disconnect.quitting`).
    pub fn quit(&self, reason: Option<&str>) {
        *self.quit_reason.lock().unwrap() =
            Some(reason.unwrap_or("disconnect.quitting").to_string());
        self.client.disconnect();
    }

    /// `bot.end(reason?)` — end the connection. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `socketClosed`).
    pub fn end(&self, reason: Option<&str>) {
        *self.quit_reason.lock().unwrap() = reason.map(str::to_string);
        self.client.disconnect();
    }

    /// `bot.respawn()` — manual respawn; a no-op while alive (upstream
    /// semantics). azalea auto-respawns on death (like mineflayer's default
    /// `respawn: true`), so this mainly exists for parity.
    pub fn respawn(&self) {
        if !self.is_alive() {
            use azalea::protocol::packets::game::s_client_command::{
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
            .get_component::<azalea::player::GameProfileComponent>()
            .map(|profile| profile.name.clone())
            .unwrap_or_default()
    }

    /// `bot.version` — the Minecraft version azalea targets (single-version
    /// port; see api-parity.md).
    pub fn version(&self) -> &'static str {
        azalea::protocol::packets::VERSION_NAME
    }

    /// `bot.protocolVersion`.
    pub fn protocol_version(&self) -> i32 {
        azalea::protocol::packets::PROTOCOL_VERSION
    }

    /// `bot.majorVersion` — single-version port: same as [`Bot::version`].
    pub fn major_version(&self) -> &'static str {
        azalea::protocol::packets::VERSION_NAME
    }

    /// `bot.registry` — a snapshot of the server's dynamic registries
    /// (dimensions, enchantments, damage kinds, …). Static data
    /// (blocks/items/entities) lives in `azalea::registry` / `azalea::block`.
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
    /// slice of the world (azalea's `PartialWorld`). 10s timeout, same as
    /// upstream.
    pub async fn wait_for_chunks_to_load(&self) -> Result<()> {
        const TIMEOUT: Duration = Duration::from_secs(10);
        tokio::time::timeout(TIMEOUT, async {
            let mut rx = self.events();
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

    /// Whether we've received the login packet and are in the game state.
    /// Unlike `azalea::Client::logged_in` this never panics — it returns
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

    #[test]
    fn bot_event_broadcast_reaches_all_subscribers() {
        let (tx, mut rx1) = broadcast::channel::<BotEvent>(16);
        let mut rx2 = tx.subscribe();

        tx.send(BotEvent::Login).unwrap();
        tx.send(BotEvent::Kicked("bye".to_string())).unwrap();

        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Kicked(_)));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Kicked(_)));
    }

    #[test]
    fn builder_defaults() {
        let b = Bot::builder();
        assert_eq!(b.host, "localhost");
        assert_eq!(b.port, 25565);
        assert_eq!(b.username, "Player");
        assert!(b.auth.is_none());
        assert!(b.view_distance.is_none());
        let b = b
            .host("example.org")
            .port(25566)
            .username("x")
            .view_distance(4);
        assert_eq!(b.host, "example.org");
        assert_eq!(b.port, 25566);
        assert_eq!(b.username, "x");
        assert_eq!(b.view_distance, Some(4));
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
        let _ = |p: azalea::chat::ChatPacket| BotEvent::Chat(p);
        let _ = |p: Arc<ClientboundGamePacket>| BotEvent::Packet(p);
        let _ = |i: PlayerInfo| BotEvent::PlayerJoined(i);
    }
}
