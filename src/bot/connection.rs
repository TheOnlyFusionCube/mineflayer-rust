//! `createBot(options)`: the builder, the auth choice, and the bot-thread
//! connection bootstrap.

use mineflayer_engine::{
    ClientInformation, ClientPlugins, StartClientOpts,
    app::{PluginGroup, PluginGroupBuilder},
    auto_reconnect::AutoReconnectPlugin,
    entity::HumanoidArm,
    protocol::{address::ResolvableAddr as _, connect::Proxy},
    swarm::{SwarmChatPlugin, SwarmPlugin},
};

use super::*;
use crate::{
    bridge,
    misc::{ChatVisibility, ModelCustomization, ParticleStatus, ViewDistance},
};

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
    /// merged over the engine's [`ClientInformation`] defaults and sent on
    /// `Event::Init`; `None` until one of them is set.
    client_information: Option<ClientInformation>,
    /// `options.brand` — sent as the `minecraft:brand` custom payload on
    /// `Event::Login`, overwriting the `"vanilla"` the engine already sent
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
    /// Whether the bridge broadcasts [`BotEvent::Packet`] (default `true`).
    /// See [`BotBuilder::packet_events`].
    packet_events: bool,
    /// `options.agent`-analog — the engine's SOCKS5 proxy support
    /// (`Client::join_with_proxy`); also covers the `connect` proxy use case.
    proxy: Option<mineflayer_engine::protocol::connect::Proxy>,
}

/// How the bot authenticates — covers `auth`, `accessToken`, and prebuilt
/// accounts. (No password auth: Mojang password login is dead upstream too;
/// The engine uses the Microsoft device-code flow or access tokens.)
#[derive(Clone)]
pub enum Auth {
    /// `auth: 'offline'` — no Mojang login; `username` is sent as-is.
    Offline {
        /// The username presented to the server.
        username: String,
    },
    /// `auth: 'microsoft'` — the engine's interactive device-code flow (prints a
    /// link code; caches tokens in `~/.minecraft/mineflayer-auth.json`).
    Microsoft {
        /// Microsoft account email — the token cache key.
        email: String,
    },
    /// A prebuilt [`mineflayer_engine::account::Account`] — e.g.
    /// `Account::with_microsoft_access_token(token)` (the `accessToken`
    /// option) or a custom `AccountTrait` impl.
    Account(mineflayer_engine::account::Account),
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
            packet_events: true,
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
    /// The engine's default `false`.
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
    /// login packet arrives. The engine also sends `"vanilla"` earlier during
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
    /// Whether [`Bot::events`] carries [`BotEvent::Packet`], the raw
    /// clientbound-packet stream (default `true`). Pass `false` when no
    /// subscriber reads raw packets: they are about 60% of all events (G1
    /// measurement, `docs/performance.md`), so a slow subscriber is far less
    /// likely to fall behind the 8192-event ring and see `Lagged`. Every
    /// derived event (`BlockUpdate`, `EntityMoved`, `WindowUpdateSlot`, …)
    /// still arrives, and the port's own waits do not read raw packets.
    pub fn packet_events(mut self, enabled: bool) -> Self {
        self.packet_events = enabled;
        self
    }
    /// `options.agent`-analog — connect through a SOCKS5 proxy
    /// (the engine's `Client::join_with_proxy`; covers the server connection,
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
    /// The connection and ECS run on a dedicated thread — the engine's ECS runner
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
                        let _ = init_tx.send(Self::connect_engine(self).await);
                    });
                    // Drive the LocalSet until all local tasks finish. The engine's
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

    /// Join the server through the engine's bootstrap with
    /// [`client_plugins`], and wire up the event bridge. Runs inside the bot
    /// thread's `LocalSet` (the engine requires it for `Client::join`).
    async fn connect_engine(builder: BotBuilder) -> Result<Bot> {
        let account = match builder.auth {
            Some(Auth::Offline { username }) => {
                mineflayer_engine::account::Account::offline(&username)
            }
            Some(Auth::Microsoft { email }) => {
                mineflayer_engine::account::Account::microsoft(&email)
                    .await
                    .map_err(Error::Auth)?
            }
            Some(Auth::Account(account)) => account,
            None => mineflayer_engine::account::Account::offline(&builder.username),
        };
        let address = format!("{}:{}", builder.host, builder.port)
            .resolve()
            .await
            .map_err(|e| Error::Connect(e.to_string()))?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (opts, _app_exit) =
            StartClientOpts::new_with_plugins(account, address, Some(tx), client_plugins());
        let opts = match builder.proxy.clone() {
            Some(proxy) => opts.proxy(proxy),
            None => opts,
        };
        let client = mineflayer_engine::Client::start_client(opts).await;

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
        // The engine queues before `join` returns — and everything since is on
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
                packet_events: builder.packet_events,
            },
            alive_rx,
            shutdown_tx,
        );

        Ok(Bot {
            client,
            bootstrap_events: Arc::new(Mutex::new(Some(bootstrap_events))),
            _alive: Some(alive),
            shutdown: shutdown_rx,
            shared: Arc::new(shared),
            chat_length_limit: builder.chat_length_limit,
        })
    }
}

/// The engine's client plugins as the port runs them. Auto-reconnect is
/// off: it reconnects 5 s after any disconnect, our own `disconnect()`
/// included, which would break `end`/`quit`/`kicked`. The swarm plugins
/// are off: a bot is one client.
pub(crate) fn client_plugins() -> PluginGroupBuilder {
    ClientPlugins
        .build()
        .disable::<AutoReconnectPlugin>()
        .disable::<SwarmPlugin>()
        .disable::<SwarmChatPlugin>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-log: the crate never installs a global subscriber. The engine's
    /// `log` feature adds a log plugin that does, and a dependent's own
    /// `tracing_subscriber::…::init()` after `connect` then panics.
    /// The check reads process-global state: no unit test in this crate
    /// may install a global subscriber.
    #[test]
    fn the_production_app_installs_no_global_subscriber() {
        let mut app = mineflayer_engine::app::App::new();
        app.add_plugins(client_plugins());
        app.finish();
        assert!(!tracing::dispatcher::has_been_set());
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
        // untouched fields keep the engine's defaults (documented divergence
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
}
