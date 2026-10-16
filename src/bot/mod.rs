//! `Bot` — the connected bot handle, the Rust analog of the object mineflayer's
//! `createBot()` returns.
//!
//! [`Bot::builder`] / [`BotBuilder::connect`] are the `createBot(options)`
//! analog. Events are delivered over a [`tokio::sync::broadcast`] channel
//! (the multi-subscriber, lag-tolerant EventEmitter analog): call
//! [`Bot::events`] to subscribe.

use std::{sync::Arc, time::Duration};

use azalea::{
    ClientInformation,
    auto_reconnect::AutoReconnectDelay,
    core::{position::ChunkPos, registry_holder::RegistryHolder},
    entity::HumanoidArm,
    protocol::connect::Proxy,
    world::WorldName,
};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{broadcast, watch};

use crate::{
    Error, Result, bridge,
    chat::ChatShared,
    misc::{ChatVisibility, ModelCustomization, ParticleStatus, ViewDistance},
    world::WorldState,
};

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
/// `connect_azalea`, `Bot::new`, and the bridge's dependency bag. Each
/// domain keeps its own lock; nothing here is a single global lock.
#[derive(Clone)]
pub(crate) struct BotShared {
    /// The bot's event stream (see [`Bot::events`]).
    pub events: broadcast::Sender<BotEvent>,
    /// Reason passed to [`Bot::quit`]/[`Bot::end`]; the bridge consumes it
    /// when azalea reports the disconnect so `BotEvent::End` can carry it
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
    /// task — azalea drops most of these packets upstream.
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
}

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
            events,
        }
    }
}

/// `BotEvent` broadcast ring size, shared by every `Bot`. A busy world
/// emits several thousand events a second (one `Packet` per clientbound
/// packet plus per-entity domain events), so 256 overflowed mid-burst
/// and lagged subscribers silently missed events; 8192 holds a
/// multi-second flood without the multi-MB upfront ring a much larger
/// capacity would allocate per `Bot`.
mod event;
pub use event::BotEvent;

const EVENT_CHANNEL_CAPACITY: usize = 8192;

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
    /// The `options.*` client-information fields (`dialogDistance`/
    /// `viewDistance`, `chat`, `colorsEnabled`, `skinParts`, `mainHand`,
    /// `enableTextFiltering`, `enableServerListing`, `particleStatus`) —
    /// merged over azalea's [`ClientInformation`] defaults and sent on
    /// `Event::Init`; `None` until one of them is set.
    client_information: Option<ClientInformation>,
    /// `options.brand` — sent as the `minecraft:brand` custom payload on
    /// `Event::Login`, overwriting the `"vanilla"` azalea already sent
    /// during the config phase (upstream writes it on the login packet;
    /// the server keeps the last brand it received).
    brand: Option<String>,
    /// `options.difficulty` — stored client-side only, like upstream's
    /// `bot.settings.difficulty` (default `2`); the server is authoritative.
    difficulty: u8,
    /// `options.chatLengthLimit` — `chat`/`whisper` split limit
    /// (default [`crate::chat::CHAT_LENGTH_LIMIT`], 256 — mc26 always takes
    /// upstream's >=1.11 branch).
    chat_length_limit: usize,
    /// `options.defaultChatPatterns` (default `true`) — `false` suppresses
    /// [`BotEvent::Whisper`], our stand-in for upstream's deprecated default
    /// whisper patterns.
    default_chat_patterns: bool,
    /// `options.checkTimeoutInterval` — the keepalive watchdog: the bridge
    /// ends the connection with `End("keepAliveError")` when the server goes
    /// this long without a keepalive (default `30s`, upstream's default;
    /// armed on the first keepalive like upstream).
    check_timeout_interval: Duration,
    /// `options.agent`-analog — azalea's SOCKS5 proxy support
    /// (`Client::join_with_proxy`); also covers the `connect` proxy use case.
    proxy: Option<azalea::protocol::connect::Proxy>,
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
            client_information: None,
            brand: None,
            difficulty: 2,
            chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
            default_chat_patterns: true,
            check_timeout_interval: Duration::from_secs(30),
            proxy: None,
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
    /// `options.viewDistance` — a chunk count or an upstream preset
    /// ([`ViewDistance::Far`]/[`ViewDistance::Normal`]/
    /// [`ViewDistance::Short`]/[`ViewDistance::Tiny`] → 12/10/8/6).
    pub fn view_distance(mut self, view_distance: impl Into<ViewDistance>) -> Self {
        self.client_information_mut().view_distance = view_distance.into().chunks();
        self
    }

    /// `options.chat` — upstream's `'enabled'`/`'commandsOnly'`/`'disabled'`
    /// as [`ChatVisibility::Full`]/[`ChatVisibility::System`]/
    /// [`ChatVisibility::Hidden`]. Sent on `Event::Init`.
    pub fn chat(mut self, chat: ChatVisibility) -> Self {
        self.client_information_mut().chat_visibility = chat;
        self
    }
    /// `options.colorsEnabled` (upstream default `true`).
    pub fn colors_enabled(mut self, enabled: bool) -> Self {
        self.client_information_mut().chat_colors = enabled;
        self
    }
    /// `options.skinParts` — the seven model-customization flags
    /// (upstream default: all shown).
    pub fn skin_parts(mut self, skin_parts: ModelCustomization) -> Self {
        self.client_information_mut().model_customization = skin_parts;
        self
    }
    /// `options.mainHand` — `'left'`/`'right'` as [`HumanoidArm`]
    /// (upstream default `'right'`).
    pub fn main_hand(mut self, main_hand: HumanoidArm) -> Self {
        self.client_information_mut().main_hand = main_hand;
        self
    }
    /// `options.enableTextFiltering` (upstream default `false`).
    pub fn enable_text_filtering(mut self, enabled: bool) -> Self {
        self.client_information_mut().text_filtering_enabled = enabled;
        self
    }
    /// `options.enableServerListing`. Divergence: upstream's initializer is
    /// `options.enableServerListing || true`, so the flag is dead and the
    /// packet always sends `true`; here the flag actually applies, over
    /// azalea's default `false`.
    pub fn enable_server_listing(mut self, enabled: bool) -> Self {
        self.client_information_mut().allows_listing = enabled;
        self
    }
    /// `options.particleStatus`. Divergence: upstream parses the option but
    /// hardcodes `'all'` into `bot.settings` — the value it accepts is never
    /// sent; here it lands on the wire like the option intends.
    pub fn particle_status(mut self, status: ParticleStatus) -> Self {
        self.client_information_mut().particle_status = status;
        self
    }
    /// `options.brand` (upstream default `'vanilla'`) — the client brand the
    /// server records, sent as the `minecraft:brand` custom payload once the
    /// login packet arrives. azalea also sends `"vanilla"` earlier during
    /// the config phase; the server keeps the last brand received, so this
    /// wins.
    pub fn brand(mut self, brand: impl Into<String>) -> Self {
        self.brand = Some(brand.into());
        self
    }
    /// `options.difficulty` (upstream default `2`) — stored client-side
    /// only, readable as `bot.settings().difficulty`; never sent (the
    /// server is authoritative, same as upstream).
    pub fn difficulty(mut self, difficulty: u8) -> Self {
        self.difficulty = difficulty;
        self
    }
    /// `options.chatLengthLimit` — the limit `chat`/`whisper` split outgoing
    /// messages to (upstream default `256` on modern versions).
    pub fn chat_length_limit(mut self, limit: usize) -> Self {
        self.chat_length_limit = limit;
        self
    }
    /// `options.defaultChatPatterns` (upstream default `true`). Pass `false`
    /// to suppress [`BotEvent::Whisper`] — the event our port produces in
    /// place of upstream's deprecated default whisper patterns.
    /// [`BotEvent::Chat`] is unaffected: it is the raw chat-packet stream,
    /// not a pattern product.
    pub fn default_chat_patterns(mut self, enabled: bool) -> Self {
        self.default_chat_patterns = enabled;
        self
    }
    /// `options.checkTimeoutInterval` (upstream default `30*1000`ms) — how
    /// long the server may go without a keepalive before the bridge ends
    /// the connection, emitting `BotEvent::Error` then
    /// [`BotEvent::End`] with `"keepAliveError"` like upstream. The watchdog
    /// arms on the first keepalive received, matching upstream's per-
    /// keepalive `setTimeout` re-arm.
    pub fn check_timeout_interval(mut self, mut interval: Duration) -> Self {
        // upstream's `options.checkTimeoutInterval || 30*1000`: a falsy
        // value means the default, so ZERO selects it rather than an
        // instant timeout
        if interval.is_zero() {
            interval = Duration::from_secs(30);
        }
        self.check_timeout_interval = interval;
        self
    }
    /// `options.agent`-analog — connect through a SOCKS5 proxy
    /// (azalea's `Client::join_with_proxy`; covers the server connection,
    /// not Mojang session auth).
    pub fn proxy(mut self, proxy: Proxy) -> Self {
        self.proxy = Some(proxy);
        self
    }

    fn client_information_mut(&mut self) -> &mut ClientInformation {
        self.client_information
            .get_or_insert_with(ClientInformation::default)
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
        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let thread_name = format!("mineflayer-bot-{}", self.username);
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("mineflayer: build bot runtime");
                rt.block_on(async {
                    let local = tokio::task::LocalSet::new();
                    local.spawn_local(async move {
                        let _ = init_tx.send(Self::connect_azalea(self).await);
                    });
                    // Drive the LocalSet until all local tasks finish. azalea's
                    // ECS runner is a local task; it exits when the client does
                    // (last `Bot` dropped → `client.exit()`, or fatal error).
                    local.await;
                });
            })
            .map_err(|_| Error::Runtime("could not spawn the bot thread".into()))?;

        init_rx
            .await
            .map_err(|_| Error::Runtime("bot thread exited during connect".into()))?
    }

    /// Join the server and wire up the event bridge. Runs inside the bot
    /// thread's `LocalSet` (azalea requires it for `Client::join`).
    ///
    /// This mirrors azalea's own `StartClientOpts::new` + `Client::join`
    /// bootstrap (same default plugins, same runner start) so the client app
    /// can also carry the port's [`crate::interact::DigAbortPlugin`], which
    /// cancels engine mining for aborted digs inside the tick that a pending
    /// start materializes — azalea gives no hook for that otherwise.
    async fn connect_azalea(builder: BotBuilder) -> Result<Bot> {
        let account = match builder.auth {
            Some(Auth::Offline { username }) => azalea::account::Account::offline(&username),
            Some(Auth::Microsoft { email }) => azalea::account::Account::microsoft(&email)
                .await
                .map_err(Error::Auth)?,
            Some(Auth::Account(account)) => account,
            None => azalea::account::Account::offline(&builder.username),
        };
        let address = format!("{}:{}", builder.host, builder.port);
        let (client, rx) = join_azalea(account, &address, builder.proxy.clone()).await?;

        // azalea's default plugins auto-reconnect 5s after any disconnect —
        // including our own `disconnect()`. That would silently break
        // `end`/`quit`/`kicked` semantics, so opt out for this client
        // (Duration::MAX is azalea's documented "disabled" value).
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(AutoReconnectDelay::new(Duration::MAX));

        // the misc domains keep their state in one component so it exists
        // before the first packet arrives (difficulty comes from
        // `options.difficulty`; it's client-side only like upstream)
        let mut misc_state = crate::misc::MiscState::default();
        misc_state.difficulty = builder.difficulty;
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(misc_state);

        // dig bookkeeping (lastDigTime / abort generations) — same rationale
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(crate::interact::InteractShared::default());

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let shared = BotShared::new(events);
        shared.chat.lock().default_chat_patterns = builder.default_chat_patterns;
        let (alive, alive_rx) = watch::channel(());
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        // subscribed before the bridge starts, so `Event::Init` — which
        // azalea queues before `join` returns — and everything since is on
        // the receiver the first `Bot::events()` call hands out.
        let bootstrap_events = shared.events.subscribe();
        bridge::spawn(
            client.clone(),
            rx,
            shared.clone(),
            bridge::BridgeOptions {
                client_information: builder.client_information,
                brand: builder.brand,
                check_timeout_interval: builder.check_timeout_interval,
            },
            alive_rx,
            shutdown_tx,
        );

        Ok(Bot {
            client,
            bootstrap_events: Arc::new(Mutex::new(Some(bootstrap_events))),
            _alive: Some(alive),
            shutdown: shutdown_rx,
            _shutdown_tx: None,
            shared: Arc::new(shared),
            chat_length_limit: builder.chat_length_limit,
        })
    }
}

/// Join a server through azalea's own bootstrap, plus the port's plugins.
///
/// Mirrors `StartClientOpts::new` (`DefaultPlugins`, `DefaultBotPlugins`,
/// `DefaultSwarmPlugins`, then the ECS runner) so the client app can carry
/// [`crate::interact::DigAbortPlugin`] — `Client::join`/`join_with_proxy`
/// build the same app, but offer no plugin hook.
async fn join_azalea(
    account: azalea::account::Account,
    address: &str,
    proxy: Option<azalea::protocol::connect::Proxy>,
) -> Result<(
    azalea::Client,
    tokio::sync::mpsc::UnboundedReceiver<azalea::Event>,
)> {
    use azalea::protocol::address::ResolvableAddr as _;

    let address = address
        .resolve()
        .await
        .map_err(|e| Error::Connect(e.to_string()))?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // app construction (default plugins + the dig-abort reconciler) lives
    // in `client_app` so tests run the production wiring — see its contract
    let mut app = crate::client_app::app();
    let (ecs_lock, start_running_systems, _appexit_rx) = azalea::start_ecs_runner(app.main_mut());
    start_running_systems();
    let client = azalea::Client::start_client(azalea::StartClientOpts {
        ecs_lock,
        account,
        connect_opts: azalea::join::ConnectOpts {
            address,
            server_proxy: proxy.clone(),
            sessionserver_proxy: proxy,
        },
        event_sender: Some(tx),
    })
    .await;
    Ok((client, rx))
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

    /// The underlying [`azalea::Client`] — the `bot._client` analog for when
    /// the facade doesn't cover something yet.
    pub fn client(&self) -> &azalea::Client {
        &self.client
    }

    /// Subscribe to this bot's event stream.
    ///
    /// The first call returns a receiver that was subscribed before the
    /// event bridge started — upstream's `createBot` lets listeners attach
    /// before anything fires, and this preserves that for the earliest
    /// events (`Init` is already queued in azalea's channel when `connect`
    /// returns, so a plain subscribe would race and lose it). Later calls
    /// see only events sent after they subscribe.
    ///
    /// Each call returns an independent receiver; slow consumers see
    /// [`broadcast::error::RecvError::Lagged`] rather than blocking the bot.
    ///
    /// Crate authors: inside the crate, use the private `subscribe` for
    /// waits. This method is the public API, and the first call hands out
    /// the bootstrap receiver.
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
        chat.lock().add_pattern(
            "probe",
            vec![regex::Regex::new("probe").unwrap()],
            crate::ChatPatternOpts::default(),
        );
        let events = chat
            .lock()
            .message_events(&azalea::client_chat::ChatPacket::new("probe"));
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
        // azalea can queue Init before `connect` returns; the first
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
    fn builder_defaults() {
        let b = Bot::builder();
        assert_eq!(b.host, "localhost");
        assert_eq!(b.port, 25565);
        assert_eq!(b.username, "Player");
        assert!(b.auth.is_none());
        assert!(b.client_information.is_none());
        let b = b
            .host("example.org")
            .port(25566)
            .username("x")
            .view_distance(4);
        assert_eq!(b.host, "example.org");
        assert_eq!(b.port, 25566);
        assert_eq!(b.username, "x");
        assert_eq!(b.client_information.as_ref().unwrap().view_distance, 4);
        // upstream's named presets resolve to their chunk counts
        assert_eq!(
            b.view_distance(ViewDistance::Far)
                .client_information
                .unwrap()
                .view_distance,
            12
        );
    }

    #[test]
    fn builder_options_defaults() {
        let b = Bot::builder();
        // options.* defaults — nothing merged into ClientInformation until
        // a wire-field option is set; upstream's stored defaults for the rest.
        assert!(b.client_information.is_none());
        assert!(b.brand.is_none());
        assert_eq!(b.difficulty, 2);
        assert_eq!(b.chat_length_limit, 256);
        assert!(b.default_chat_patterns);
        assert_eq!(b.check_timeout_interval, Duration::from_secs(30));
        assert!(b.proxy.is_none());
    }

    #[test]
    fn builder_options_merge_into_client_information() {
        let b = Bot::builder()
            .chat(ChatVisibility::Hidden)
            .colors_enabled(false)
            .main_hand(HumanoidArm::Left)
            .enable_text_filtering(true)
            .enable_server_listing(true)
            .particle_status(ParticleStatus::Minimal)
            .skin_parts(ModelCustomization {
                cape: false,
                ..ModelCustomization::default()
            });
        let info = b.client_information.expect("ci options should merge");
        assert_eq!(info.chat_visibility, ChatVisibility::Hidden);
        assert!(!info.chat_colors);
        assert_eq!(info.main_hand, HumanoidArm::Left);
        assert!(info.text_filtering_enabled);
        assert!(info.allows_listing);
        assert_eq!(info.particle_status, ParticleStatus::Minimal);
        assert!(!info.model_customization.cape);
        assert!(info.model_customization.jacket);
        // untouched fields keep azalea's defaults (documented divergence
        // from upstream for view_distance)
        assert_eq!(info.view_distance, 8);
        assert_eq!(info.language, "en_us");
    }

    #[test]
    fn builder_view_distance_is_a_client_information_field() {
        // both setter orders land in the one stored struct: the field the
        // bridge sends on `Event::Init` is the only representation
        let b = Bot::builder()
            .chat(ChatVisibility::Hidden)
            .view_distance(ViewDistance::Far);
        let info = b.client_information.as_ref().unwrap();
        assert_eq!(info.view_distance, 12);
        assert_eq!(info.chat_visibility, ChatVisibility::Hidden);
        let b = Bot::builder()
            .view_distance(ViewDistance::Tiny)
            .chat(ChatVisibility::System);
        let info = b.client_information.as_ref().unwrap();
        assert_eq!(info.view_distance, 6);
        assert_eq!(info.chat_visibility, ChatVisibility::System);
    }

    #[test]
    fn builder_stored_options() {
        let b = Bot::builder()
            .brand("mineflayer-rs")
            .difficulty(3)
            .chat_length_limit(100)
            .default_chat_patterns(false)
            .check_timeout_interval(Duration::from_secs(10));
        assert_eq!(b.brand.as_deref(), Some("mineflayer-rs"));
        assert_eq!(b.difficulty, 3);
        assert_eq!(b.chat_length_limit, 100);
        assert!(!b.default_chat_patterns);
        assert_eq!(b.check_timeout_interval, Duration::from_secs(10));
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
        let _ = |p: azalea::client_chat::ChatPacket| BotEvent::Chat(p);
        let _ =
            |p: Arc<azalea::protocol::packets::game::ClientboundGamePacket>| BotEvent::Packet(p);
        let _ = |i: azalea::player::PlayerInfo| BotEvent::PlayerJoined(i);
    }
}
