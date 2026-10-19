//! One keep-online connection: log in, answer the server's liveness
//! traffic, decline resource packs, respawn on death, relay chat both ways.
//! Nothing about the world is kept.
//!
//! A session is two tokio tasks. The reader task owns the socket's read half
//! and forwards whole frames. The session task owns the write half and the
//! state, and selects between those frames and the [`Command`]s of its
//! [`Handle`]. Login runs before the split, so compression and encryption
//! switch on exactly at the packet that turns them on.
//!
//! Every wait is bounded: connecting, joining, each write, and the silence
//! between two frames (servers send a keepalive at least every 15 s, and the
//! vanilla client gives up after 30 s). Lifecycle goes through a `watch`
//! channel and chat through a bounded queue that drops when full, so an owner
//! that stops reading never stalls the connection.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mineflayer_auth::{
    certs::{Certificates, fetch_certificates_from},
    sessionserver::{self, ClientSessionServerError, SessionServerJoinOpts},
};
use mineflayer_crypto::signing;
use mineflayer_protocol::{
    address::{ResolvedAddr, ServerAddr},
    connect::{Connection, RawReadConnection, RawWriteConnection},
    read::ReadPacketError,
    resolve::ResolveError,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, timeout, timeout_at},
};
use uuid::Uuid;

use crate::{
    fields,
    proxy::Proxy,
    protodef::{self, Value},
    table::{self, Dialect, PacketError, State},
    text,
};

pub struct Options {
    /// The address as the player typed it; sent in the handshake.
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: Auth,
    /// A SOCKS5, SOCKS4 or HTTP proxy for the game connection. The
    /// session-server join goes direct, as the vanilla client's does.
    pub proxy: Option<Proxy>,
    /// The protocol to speak; `None` asks the server first ([`ping`]).
    pub protocol: Option<i32>,
    pub view_distance: u8,
}

pub enum Auth {
    /// Joins offline-mode servers only.
    Offline,
    /// A Minecraft access token and the profile it belongs to.
    Online { access_token: String, uuid: Uuid, servers: AuthServers },
}

/// Where an online account authenticates: Mojang's servers, or an
/// authlib-injector (Yggdrasil) network's.
#[derive(Clone, Debug)]
pub struct AuthServers {
    /// The session server, which the client tells it is joining.
    pub session: String,
    /// The Minecraft services API, which issues chat-signing certificates.
    pub services: String,
}

impl Default for AuthServers {
    fn default() -> Self {
        Self {
            session: sessionserver::MOJANG_SESSION_SERVER.to_owned(),
            services: mineflayer_auth::certs::MOJANG_SERVICES.to_owned(),
        }
    }
}

impl AuthServers {
    /// The servers of an authlib-injector Yggdrasil API rooted at `root`.
    pub fn yggdrasil(root: &str) -> Self {
        let root = root.trim_end_matches('/');
        Self { session: format!("{root}/sessionserver"), services: format!("{root}/minecraftservices") }
    }
}

/// TCP connect plus the proxy handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// From the TCP connect to the player standing in the world.
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(60);
/// Silence on the connection before the session gives up, as the vanilla
/// client's read timeout.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// One write blocked this long means the server stopped reading.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a session is. The latest value is always readable from
/// [`Handle::status`]; a session that has ended stays `Ended`.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    /// The player is in the world (the first teleport arrived).
    Online { protocol: i32 },
    Ended(End),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A chat or system message, as plain text. Dropped when the receiver
    /// lags rather than stalling the connection.
    Chat(String),
    Health(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum End {
    Kicked(String),
    Failed(String),
    /// The handle asked to quit, or every handle was dropped.
    Quit,
}

#[derive(Debug)]
pub enum Command {
    /// A chat message, or a command when it starts with `/`.
    Chat(String),
    /// Swing the main hand.
    Swing,
    /// Turn the head by these degrees; pitch stays within ±90.
    Rotate { yaw: f32, pitch: f32 },
    /// Hold or release sneak.
    Sneak(bool),
    Quit,
}

#[derive(Clone)]
pub struct Handle {
    commands: mpsc::Sender<Command>,
    status: watch::Receiver<Status>,
}

impl Handle {
    /// Queue a command; `false` when the session has ended.
    pub async fn send(&self, c: Command) -> bool {
        self.commands.send(c).await.is_ok()
    }

    /// The session's status; `changed()` on it waits for the next one.
    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("resolve: {0}")]
    Resolve(#[from] ResolveError),
    #[error("connect: {0}")]
    Io(#[from] std::io::Error),
    #[error("read: {0}")]
    Read(#[from] Box<ReadPacketError>),
    #[error("{0}")]
    Packet(#[from] PacketError),
    #[error("protocol {0} is not supported")]
    Unsupported(i32),
    #[error("the server is in online mode and the session has no account")]
    NeedsAccount,
    #[error("session server: {0}")]
    SessionServer(#[from] ClientSessionServerError),
    #[error("encryption: {0}")]
    Crypto(String),
    #[error("bad status response: {0}")]
    Status(String),
    #[error("kicked: {0}")]
    Kicked(String),
    #[error("{0} timed out")]
    Timeout(&'static str),
}

/// Start a session on the current tokio runtime.
pub fn spawn(opts: Options) -> (Handle, mpsc::Receiver<Event>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (ev_tx, ev_rx) = mpsc::channel(32);
    let (status_tx, status_rx) = watch::channel(Status::Connecting);
    tokio::spawn(async move {
        let end = match run(opts, cmd_rx, &ev_tx, &status_tx).await {
            Ok(end) => end,
            Err(Error::Kicked(reason)) => End::Kicked(reason),
            Err(e) => End::Failed(e.to_string()),
        };
        status_tx.send_replace(Status::Ended(end));
    });
    (Handle { commands: cmd_tx, status: status_rx }, ev_rx)
}

async fn open(addr: &ResolvedAddr, proxy: Option<&Proxy>) -> Result<(RawReadConnection, RawWriteConnection), Error> {
    let conn = timeout(CONNECT_TIMEOUT, async {
        let stream = match proxy {
            Some(p) => p.connect(addr.socket).await?,
            None => {
                let s = tokio::net::TcpStream::connect(addr.socket).await?;
                s.set_nodelay(true)?;
                s
            }
        };
        Connection::new_from_stream(stream).await.map_err(|e| match e {
            mineflayer_protocol::connect::ConnectionError::Io(io) => io,
        })
    })
    .await
    .map_err(|_| Error::Timeout("connect"))??;
    Ok(conn.into_split_raw())
}

async fn write(w: &mut RawWriteConnection, packet: &[u8]) -> Result<(), Error> {
    timeout(WRITE_TIMEOUT, w.write(packet)).await.map_err(|_| Error::Timeout("write"))??;
    Ok(())
}

fn handshake(protocol: i32, host: &str, port: u16, next_state: i32) -> Vec<u8> {
    let mut p = vec![0];
    protodef::write_varint(&mut p, protocol);
    protodef::write_varint(&mut p, host.len() as i32);
    p.extend(host.as_bytes());
    p.extend(port.to_be_bytes());
    protodef::write_varint(&mut p, next_state);
    p
}

/// Ask the server which protocol it speaks (the server list ping).
pub async fn ping(host: &str, port: u16, proxy: Option<&Proxy>) -> Result<i32, Error> {
    let addr = ResolvedAddr::new(ServerAddr { host: host.to_owned(), port }).await?;
    let (mut r, mut w) = open(&addr, proxy).await?;
    write(&mut w, &handshake(-1, host, port, 1)).await?;
    write(&mut w, &[0]).await?;
    let raw = timeout(READ_TIMEOUT, r.read()).await.map_err(|_| Error::Timeout("status"))??;
    let v = protodef::Type::Container(vec![("id".into(), protodef::Type::VarInt), ("json".into(), protodef::Type::String)])
        .decode(&mut std::io::Cursor::new(&raw[..]))
        .map_err(|e| Error::Status(e.to_string()))?;
    let Some(Value::Str(json)) = v.get("json") else {
        return Err(Error::Status("no JSON".into()));
    };
    let status: serde_json::Value = serde_json::from_str(json).map_err(|e| Error::Status(e.to_string()))?;
    status["version"]["protocol"]
        .as_i64()
        .map(|p| p as i32)
        .ok_or_else(|| Error::Status("no version.protocol".into()))
}

fn bytes(v: &Value, name: &str) -> Vec<u8> {
    match v.get(name) {
        Some(Value::Bytes(b)) => b.clone(),
        _ => Vec::new(),
    }
}

/// The client brand plugin message the vanilla client sends on join; some
/// proxies and anti-bot plugins kick a client that never sends one.
fn brand(d: &Dialect) -> Value {
    let channel = if d.protocol <= 340 { "MC|Brand" } else { "minecraft:brand" };
    let mut data = Vec::new();
    protodef::write_varint(&mut data, 7);
    data.extend(b"vanilla");
    Value::Map(vec![("channel".into(), Value::Str(channel.into())), ("data".into(), Value::Bytes(data))])
}

/// The text a server accepts in a chat packet: no section sign, no control
/// characters (vanilla disconnects with "illegal characters"), and at most
/// `limit` UTF-16 units, which is how the server measures the length.
fn chat_text(text: &str, limit: usize) -> String {
    let mut units = 0;
    text.chars()
        .filter(|&c| c != '\u{a7}' && c >= ' ' && c != '\u{7f}')
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect()
}

fn settings(view_distance: u8) -> Value {
    fields! {
        "locale" => "en_us",
        "viewDistance" => i64::from(view_distance),
        "chatFlags" => 0i64,
        "chatColors" => true,
        "skinParts" => 0i64,
        "mainHand" => 1i64,
        "enableTextFiltering" => false,
        "enableServerListing" => true,
        "particleStatus" => "minimal",
    }
}

/// A server may move the player on with a transfer (1.20.5+); a chain of
/// transfers that never reaches the world ends the session.
const MAX_TRANSFERS_WITHOUT_JOIN: u32 = 5;

/// Cookies a server stored with store_cookie, offered back to the next server
/// after a transfer. Vanilla caps a cookie at 5 KiB.
#[derive(Default)]
struct Cookies(Vec<(String, Vec<u8>)>);

impl Cookies {
    const MAX_LEN: usize = 5 * 1024;
    const MAX_COUNT: usize = 32;

    fn store(&mut self, key: String, value: Vec<u8>) {
        if value.len() > Self::MAX_LEN {
            return;
        }
        self.0.retain(|(k, _)| *k != key);
        if self.0.len() < Self::MAX_COUNT {
            self.0.push((key, value));
        }
    }

    fn response(&self, request: &Value) -> Value {
        let key = request.get("cookie").cloned().unwrap_or(Value::None);
        let value = match &key {
            Value::Str(k) => self.0.iter().find(|(c, _)| c == k).map_or(Value::None, |(_, v)| Value::Bytes(v.clone())),
            _ => Value::None,
        };
        Value::Map(vec![("key".into(), key), ("value".into(), value)])
    }
}

/// Chat signing from 1.19.3 (protocol 761): the player's certificate from
/// the services API, a chat session id, and the index of the next signature.
struct Signer {
    certs: Certificates,
    player: Uuid,
    session: Uuid,
    index: u32,
}

impl Signer {
    fn new(certs: Certificates, player: Uuid) -> Self {
        let mut random = [0u8; 16];
        random[..8].copy_from_slice(&signing::make_salt().to_be_bytes());
        random[8..].copy_from_slice(&signing::make_salt().to_be_bytes());
        Self { certs, player, session: uuid::Builder::from_random_bytes(random).into_uuid(), index: 0 }
    }

    fn session_update(&self) -> Value {
        Value::Map(vec![
            ("sessionUUID".into(), Value::Uuid(self.session.as_u128())),
            ("expireTime".into(), Value::Int(self.certs.expires_at.timestamp_millis())),
            ("publicKey".into(), Value::Bytes(self.certs.public_key_der.clone())),
            ("signature".into(), Value::Bytes(self.certs.signature_v2.clone())),
        ])
    }

    /// Sign `message` sent at `timestamp` with `salt`, acknowledging no
    /// earlier messages: vanilla's last-seen validator lets a client leave
    /// every pending message unacknowledged.
    fn sign(&mut self, message: &str, salt: u64, timestamp: SystemTime) -> Vec<u8> {
        let signature = signing::sign_chat_message(&signing::SignChatMessageOptions {
            account_uuid: self.player,
            chat_session_uuid: self.session,
            message_index: self.index,
            salt,
            timestamp,
            message: message.to_owned(),
            private_key: self.certs.private_key.clone(),
        });
        self.index += 1;
        signature.bytes.to_vec()
    }
}

/// The arguments of a vanilla command that the server verifies a signature
/// for (brigadier `message` arguments), by argument name.
fn signable_arguments(command: &str) -> Vec<(&'static str, String)> {
    let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
    let (arg, value) = match name.trim_start_matches("minecraft:") {
        "msg" | "tell" | "w" => ("message", rest.split_once(' ').map_or("", |(_, m)| m)),
        "teammsg" | "tm" | "say" => ("message", rest),
        "me" => ("action", rest),
        _ => return Vec::new(),
    };
    if value.is_empty() { Vec::new() } else { vec![(arg, value.to_owned())] }
}

/// How one connection ended.
enum Next {
    End(End),
    Transfer { host: String, port: u16 },
}

async fn run(
    opts: Options,
    mut cmd_rx: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    status: &watch::Sender<Status>,
) -> Result<End, Error> {
    let protocol = match opts.protocol {
        Some(p) => p,
        None => ping(&opts.host, opts.port, opts.proxy.as_ref()).await?,
    };
    let d = table::dialect(protocol).ok_or(Error::Unsupported(protocol))?;
    let mut target = ServerAddr { host: opts.host.clone(), port: opts.port };
    let mut cookies = Cookies::default();
    let mut certs: Option<Certificates> = None;
    let mut transferred = false;
    let mut transfers_without_join = 0;
    loop {
        let mut joined = false;
        let next =
            connection(d, &opts, &target, transferred, &mut cookies, &mut certs, &mut cmd_rx, events, status, &mut joined).await?;
        match next {
            Next::End(end) => return Ok(end),
            Next::Transfer { host, port } => {
                transfers_without_join = if joined { 1 } else { transfers_without_join + 1 };
                if transfers_without_join > MAX_TRANSFERS_WITHOUT_JOIN {
                    return Err(Error::Timeout("transfer chain"));
                }
                status.send_replace(Status::Connecting);
                target = ServerAddr { host, port };
                transferred = true;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connection(
    d: &'static Dialect,
    opts: &Options,
    target: &ServerAddr,
    transferred: bool,
    cookies: &mut Cookies,
    certs: &mut Option<Certificates>,
    cmd_rx: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    status: &watch::Sender<Status>,
    joined: &mut bool,
) -> Result<Next, Error> {
    let addr = ResolvedAddr::new(target.clone()).await?;
    let (mut r, mut w) = open(&addr, opts.proxy.as_ref()).await?;
    let join_deadline = Instant::now() + JOIN_TIMEOUT;

    let state = timeout_at(join_deadline, login(d, opts, target, transferred, cookies, &mut r, &mut w))
        .await
        .map_err(|_| Error::Timeout("join"))??;

    let (frame_tx, mut frames) = mpsc::channel::<Result<Box<[u8]>, String>>(16);
    let reader = tokio::spawn(read_frames(d, r, frame_tx));
    let mut s = Session {
        d,
        w,
        state,
        view_distance: opts.view_distance,
        online: false,
        unacked: 0,
        settings_sent: false,
        loaded_pending: true,
        position: None,
        entity_id: 0,
        on_ground: true,
        signer: None,
        session_sent: false,
    };
    // The certificate comes from the services API while the player joins;
    // without it chat stays unsigned, as on any server that does not
    // enforce secure profiles.
    let mut fetch = match (&opts.auth, certs.as_ref()) {
        (Auth::Online { uuid, .. }, Some(c)) if d.protocol >= 761 => {
            s.signer = Some(Signer::new(c.clone(), *uuid));
            None
        }
        (Auth::Online { access_token, servers, .. }, None) if d.protocol >= 761 => {
            let (token, services) = (access_token.clone(), servers.services.clone());
            Some(tokio::spawn(async move { fetch_certificates_from(&services, &token).await }))
        }
        _ => None,
    };
    if s.state == State::Configuration {
        s.send(State::Configuration, "custom_payload", &brand(d)).await?;
        s.send(State::Configuration, "settings", &settings(s.view_distance)).await?;
        s.settings_sent = true;
    }

    let mut last_frame = Instant::now();
    let result = loop {
        let deadline = if s.online { last_frame + READ_TIMEOUT } else { join_deadline.min(last_frame + READ_TIMEOUT) };
        // checked here as well as in the select: a server that always has a
        // frame ready would otherwise starve the timer branch
        if Instant::now() >= deadline {
            break Err(Error::Timeout(if s.online { "read" } else { "join" }));
        }
        tokio::select! {
            biased;
            fetched = async { fetch.as_mut().expect("guarded").await }, if fetch.is_some() => {
                fetch = None;
                match fetched {
                    Ok(Ok(c)) => {
                        if let Auth::Online { uuid, .. } = &opts.auth {
                            s.signer = Some(Signer::new(c.clone(), *uuid));
                        }
                        *certs = Some(c);
                        s.start_chat_session().await?;
                    }
                    Ok(Err(e)) => tracing::warn!("no chat-signing certificate, chat stays unsigned: {e}"),
                    Err(e) => tracing::warn!("certificate fetch failed: {e}"),
                }
            },
            cmd = cmd_rx.recv() => match cmd {
                None | Some(Command::Quit) => break Ok(Next::End(End::Quit)),
                Some(c) => s.command(c).await?,
            },
            frame = frames.recv() => match frame {
                Some(Ok(raw)) => {
                    last_frame = Instant::now();
                    let was_online = s.online;
                    if let Some(next) = s.handle(&raw, events, cookies).await? {
                        break Ok(next);
                    }
                    if s.online && !was_online {
                        *joined = true;
                        s.start_chat_session().await?;
                        status.send_replace(Status::Online { protocol: d.protocol });
                    }
                }
                Some(Err(e)) => break Ok(Next::End(End::Failed(format!("read: {e}")))),
                None => break Ok(Next::End(End::Failed("connection closed".into()))),
            },
            () = tokio::time::sleep_until(deadline) => {
                break Err(Error::Timeout(if s.online { "read" } else { "join" }));
            }
        }
    };
    reader.abort();
    result
}

async fn login(
    d: &'static Dialect,
    opts: &Options,
    target: &ServerAddr,
    transferred: bool,
    cookies: &Cookies,
    r: &mut RawReadConnection,
    w: &mut RawWriteConnection,
) -> Result<State, Error> {
    const LOGIN: i32 = 2;
    const TRANSFER: i32 = 3;
    write(w, &handshake(d.protocol, &target.host, target.port, if transferred { TRANSFER } else { LOGIN })).await?;
    let uuid = match &opts.auth {
        Auth::Offline => mineflayer_crypto::offline::generate_uuid(&opts.username),
        Auth::Online { uuid, .. } => *uuid,
    };
    write(w, &d.encode(State::Login, "login_start", &fields! { "username" => opts.username.as_str(), "playerUUID" => uuid })?)
        .await?;
    loop {
        let raw = r.read().await?;
        let Some((name, v)) = d.decode(State::Login, &raw)? else {
            continue;
        };
        match name {
            "compress" => {
                let t = v.int("threshold").unwrap_or(-1);
                let t = u32::try_from(t).ok();
                r.compression_threshold = t;
                w.compression_threshold = t;
            }
            "encryption_begin" => {
                let Auth::Online { access_token, uuid, servers } = &opts.auth else {
                    return Err(Error::NeedsAccount);
                };
                let public_key = bytes(&v, "publicKey");
                let enc = mineflayer_crypto::encrypt(&public_key, &bytes(&v, "verifyToken")).map_err(Error::Crypto)?;
                if v.get("shouldAuthenticate") != Some(&Value::Bool(false)) {
                    let server_id = match v.get("serverId") {
                        Some(Value::Str(s)) => s.as_str(),
                        _ => "",
                    };
                    sessionserver::join(SessionServerJoinOpts {
                        session_server: &servers.session,
                        access_token,
                        public_key: &public_key,
                        private_key: &enc.secret_key,
                        uuid,
                        server_id,
                        proxy: None,
                    })
                    .await?;
                }
                let token = Value::Bytes(enc.encrypted_challenge);
                let reply = Value::Map(vec![
                    ("sharedSecret".into(), Value::Bytes(enc.encrypted_public_key)),
                    ("verifyToken".into(), token.clone()),
                    ("hasVerifyToken".into(), Value::Bool(true)),
                    ("crypto".into(), Value::Map(vec![("verifyToken".into(), token)])),
                ]);
                write(w, &d.encode(State::Login, "encryption_begin", &reply)?).await?;
                let (enc_cipher, dec_cipher) = mineflayer_crypto::create_cipher(&enc.secret_key);
                r.dec_cipher = Some(dec_cipher);
                w.enc_cipher = Some(enc_cipher);
            }
            "success" => {
                return if d.has(State::Login, "login_acknowledged") {
                    write(w, &d.encode(State::Login, "login_acknowledged", &Value::Map(vec![]))?).await?;
                    Ok(State::Configuration)
                } else {
                    Ok(State::Play)
                };
            }
            "login_plugin_request" => {
                let reply = fields! { "messageId" => v.int("messageId").unwrap_or(0) };
                write(w, &d.encode(State::Login, "login_plugin_response", &reply)?).await?;
            }
            "cookie_request" => write(w, &d.encode(State::Login, "cookie_response", &cookies.response(&v))?).await?,
            "disconnect" => return Err(Error::Kicked(v.get("reason").map(text::plain).unwrap_or_default())),
            _ => {}
        }
    }
}

/// Forward frames until the connection fails. 1.8 can turn compression on
/// in play; the reader applies it before reading the next frame.
async fn read_frames(d: &'static Dialect, mut r: RawReadConnection, tx: mpsc::Sender<Result<Box<[u8]>, String>>) {
    let watch_compression = d.has_play_set_compression();
    loop {
        match r.read().await {
            Ok(raw) => {
                if watch_compression
                    && let Ok(Some(("set_compression", v))) = d.decode(State::Play, &raw)
                {
                    r.compression_threshold = u32::try_from(v.int("threshold").unwrap_or(-1)).ok();
                }
                if tx.send(Ok(raw)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e.to_string())).await;
                return;
            }
        }
    }
}

struct Session {
    d: &'static Dialect,
    w: RawWriteConnection,
    state: State,
    view_distance: u8,
    online: bool,
    /// Signed player messages seen since the last acknowledgement.
    unacked: i64,
    /// Before 1.20.2 the client settings go out in answer to the play login
    /// packet (see `handle`); from 1.20.2 in configuration.
    settings_sent: bool,
    /// From 1.21.4 the vanilla client sends player_loaded on the first
    /// teleport after join, respawn, and reconfiguration.
    loaded_pending: bool,
    /// x, y, z, yaw, pitch after the last teleport (relative fields
    /// applied); 1.8 echoes it, and rotation starts from its yaw and pitch.
    position: Option<[f64; 5]>,
    /// The player's entity id, from the play login packet.
    entity_id: i32,
    on_ground: bool,
    signer: Option<Signer>,
    /// chat_session_update went out on this connection.
    session_sent: bool,
}

impl Session {
    async fn send(&mut self, state: State, name: &str, v: &Value) -> Result<(), Error> {
        let packet = self.d.encode(state, name, v)?;
        write(&mut self.w, &packet).await
    }

    async fn command(&mut self, c: Command) -> Result<(), Error> {
        if self.state != State::Play || !self.online {
            return Ok(());
        }
        match c {
            Command::Chat(text) => self.chat(&text).await,
            Command::Swing => {
                if self.d.has(State::Play, "punch") {
                    return self.send(State::Play, "punch", &Value::Map(vec![])).await;
                }
                let v = fields! { "hand" => 0i64, "entityId" => self.entity_id, "animation" => 1i64 };
                self.send(State::Play, "arm_animation", &v).await
            }
            Command::Rotate { yaw, pitch } => {
                let at = self.position.get_or_insert([0.0; 5]);
                at[3] = (at[3] + f64::from(yaw)).rem_euclid(360.0);
                at[4] = (at[4] + f64::from(pitch)).clamp(-90.0, 90.0);
                let v = fields! {
                    "yaw" => at[3], "pitch" => at[4], "onGround" => self.on_ground, "flags" => i64::from(self.on_ground),
                };
                self.send(State::Play, "look", &v).await
            }
            Command::Sneak(down) => {
                if self.d.has(State::Play, "player_input") {
                    const SHIFT: i64 = 1 << 5;
                    self.send(State::Play, "player_input", &fields! { "inputs" => if down { SHIFT } else { 0 } }).await
                } else {
                    let action = if down { "start_sneaking" } else { "stop_sneaking" };
                    let v = fields! { "entityId" => self.entity_id, "actionId" => action, "jumpBoost" => 0i64 };
                    self.send(State::Play, "entity_action", &v).await
                }
            }
            Command::Quit => Ok(()),
        }
    }

    async fn chat(&mut self, text: &str) -> Result<(), Error> {
        if self.state != State::Play {
            return Ok(());
        }
        let limit = if self.d.protocol <= 210 { 100 } else { 256 };
        let text = chat_text(text, limit);
        if text.is_empty() {
            return Ok(());
        }
        let now = SystemTime::now();
        let timestamp = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let salt = signing::make_salt();
        // the messages seen since the last acknowledgement, none of them
        // acknowledged (see Signer::sign)
        let offset = if self.d.protocol >= 761 { std::mem::take(&mut self.unacked) } else { 0 };
        let signer = if self.session_sent { self.signer.as_mut() } else { None };
        if let Some(command) = text.strip_prefix('/')
            && self.d.has(State::Play, "chat_command")
        {
            let signatures: Vec<Value> = match signer {
                Some(signer) => signable_arguments(command)
                    .into_iter()
                    .map(|(name, value)| {
                        Value::Map(vec![
                            ("argumentName".into(), Value::Str(name.into())),
                            ("signature".into(), Value::Bytes(signer.sign(&value, salt, now))),
                        ])
                    })
                    .collect(),
                None => Vec::new(),
            };
            let packet = if !signatures.is_empty() && self.d.has(State::Play, "chat_command_signed") {
                "chat_command_signed"
            } else {
                "chat_command"
            };
            let v = Value::Map(vec![
                ("command".into(), Value::Str(command.into())),
                ("timestamp".into(), Value::Int(timestamp)),
                ("salt".into(), Value::Int(salt as i64)),
                ("argumentSignatures".into(), Value::List(signatures)),
                ("messageCount".into(), Value::Int(offset)),
            ]);
            return self.send(State::Play, packet, &v).await;
        }
        if self.d.has(State::Play, "chat_message") {
            let signature = signer.map_or(Value::None, |signer| Value::Bytes(signer.sign(&text, salt, now)));
            let v = Value::Map(vec![
                ("message".into(), Value::Str(text)),
                ("timestamp".into(), Value::Int(timestamp)),
                ("salt".into(), Value::Int(salt as i64)),
                ("signature".into(), signature),
                ("offset".into(), Value::Int(offset)),
            ]);
            return self.send(State::Play, "chat_message", &v).await;
        }
        self.send(State::Play, "chat", &fields! { "message" => text.as_str() }).await
    }

    async fn handle(&mut self, raw: &[u8], events: &mpsc::Sender<Event>, cookies: &mut Cookies) -> Result<Option<Next>, Error> {
        let state = self.state;
        if state == State::Play {
            match self.d.clientbound_name(state, raw) {
                // the entity id leads the login packet in every protocol; the
                // rest (registries, dimensions) is never decoded
                Some("login") => {
                    let mut cur = std::io::Cursor::new(raw);
                    let _ = protodef::read_varint(&mut cur);
                    if let Some(id) = raw.get(cur.position() as usize..cur.position() as usize + 4) {
                        self.entity_id = i32::from_be_bytes([id[0], id[1], id[2], id[3]]);
                    }
                    // Before 1.20.2 the vanilla client sends its brand and
                    // settings in answer to this packet, not sooner: a 1.8
                    // server, or ViaRewind turning settings into skin-part
                    // metadata for the player's entity, needs the join first.
                    if !self.settings_sent {
                        self.settings_sent = true;
                        self.send(State::Play, "custom_payload", &brand(self.d)).await?;
                        self.send(State::Play, "settings", &settings(self.view_distance)).await?;
                    }
                    return Ok(None);
                }
                // a dimension change or a proxy's server switch; vanilla sends
                // player_loaded again after the next teleport
                Some("respawn") => {
                    self.loaded_pending = true;
                    return Ok(None);
                }
                _ => {}
            }
        }
        // A packet that does not match its schema (a plugin's odd chat, a
        // proxy's rewrite) is skipped; ending the session over it would trade
        // one lost line for a disconnect.
        let Ok(Some((name, v))) = self.d.decode(state, raw) else {
            return Ok(None);
        };
        match (state, name) {
            (_, "keep_alive") => self.send(state, "keep_alive", &v).await?,
            (_, "ping") => self.send(state, "pong", &v).await?,
            (_, "disconnect" | "kick_disconnect") => {
                return Ok(Some(Next::End(End::Kicked(v.get("reason").map(text::plain).unwrap_or_default()))));
            }
            (_, "transfer") => {
                let host = match v.get("host") {
                    Some(Value::Str(h)) => h.clone(),
                    _ => return Ok(None),
                };
                let port = u16::try_from(v.int("port").unwrap_or(25565)).unwrap_or(25565);
                return Ok(Some(Next::Transfer { host, port }));
            }
            (_, "store_cookie") => {
                if let (Some(Value::Str(key)), Some(Value::Bytes(value))) = (v.get("key"), v.get("value")) {
                    cookies.store(key.clone(), value.clone());
                }
            }
            (_, "add_resource_pack" | "resource_pack_send") => {
                // Report every pack loaded without downloading it. A server
                // that requires a pack kicks a client that declines, and
                // before 1.17 the packet cannot say the pack is required
                // (a plugin or a ViaVersion-translated server enforces it).
                const LOADED: i64 = 0;
                const ACCEPTED: i64 = 3;
                const DOWNLOADED: i64 = 4;
                let results: &[i64] = if self.d.protocol >= 765 {
                    &[ACCEPTED, DOWNLOADED, LOADED]
                } else {
                    &[ACCEPTED, LOADED]
                };
                for &result in results {
                    let mut reply = vec![("result".into(), Value::Int(result))];
                    for key in ["uuid", "hash"] {
                        if let Some(x) = v.get(key) {
                            reply.push((key.into(), x.clone()));
                        }
                    }
                    self.send(state, "resource_pack_receive", &Value::Map(reply)).await?;
                }
            }
            (_, "cookie_request") => self.send(state, "cookie_response", &cookies.response(&v)).await?,
            (State::Configuration, "select_known_packs") => self.send(state, "select_known_packs", &v).await?,
            (State::Configuration, "code_of_conduct") => self.send(state, "accept_code_of_conduct", &Value::Map(vec![])).await?,
            (State::Configuration, "finish_configuration") => {
                self.send(state, "finish_configuration", &Value::Map(vec![])).await?;
                self.state = State::Play;
                self.loaded_pending = true;
            }
            (State::Play, "start_configuration") => {
                self.send(state, "configuration_acknowledged", &Value::Map(vec![])).await?;
                self.state = State::Configuration;
                self.send(State::Configuration, "settings", &settings(self.view_distance)).await?;
            }
            (State::Play, "position") => {
                let at = self.teleport(&v);
                if self.d.has(state, "teleport_confirm") {
                    // from 26.3 the confirmation carries the accepted position
                    let id = v.int("teleportId").unwrap_or(0);
                    let v = fields! {
                        "teleportId" => id, "x" => at[0], "y" => at[1], "z" => at[2], "yaw" => at[3], "pitch" => at[4],
                    };
                    self.send(state, "teleport_confirm", &v).await?;
                } else {
                    // 1.7 sends the eye height and wants both back: feet as
                    // y and eyes as stance (the server kicks for an "illegal
                    // stance" unless stance - y is within 0.1 to 1.65)
                    let (y, stance) = if self.d.protocol <= 5 { (at[1] - 1.62, at[1]) } else { (at[1], at[1] + 1.62) };
                    let echo = fields! {
                        "x" => at[0], "y" => y, "stance" => stance, "z" => at[2], "yaw" => at[3], "pitch" => at[4],
                        "onGround" => false,
                    };
                    self.send(state, "position_look", &echo).await?;
                }
                if self.loaded_pending {
                    self.loaded_pending = false;
                    if self.d.has(state, "player_loaded") {
                        self.send(state, "player_loaded", &Value::Map(vec![])).await?;
                    }
                }
                self.online = true;
            }
            (State::Play, "chunk_batch_finished") => {
                self.send(state, "chunk_batch_received", &fields! { "chunksPerTick" => 1.0 }).await?;
            }
            (State::Play, "update_health") => {
                let health = v.float("health").unwrap_or(20.0) as f32;
                let _ = events.try_send(Event::Health(health));
                if health <= 0.0 {
                    let respawn = fields! { "actionId" => "perform_respawn", "payload" => "perform_respawn" };
                    self.send(state, "client_command", &respawn).await?;
                    self.loaded_pending = true;
                }
            }
            (State::Play, "chat") => {
                // position 2 is the action bar
                if v.int("position") != Some(2) {
                    let _ = events.try_send(Event::Chat(v.get("message").map(text::plain).unwrap_or_default()));
                }
            }
            (State::Play, "system_chat") => {
                let action_bar = v.get("isActionBar") == Some(&Value::Bool(true)) || v.int("type") == Some(2);
                if !action_bar {
                    let _ = events.try_send(Event::Chat(v.get("content").map(text::plain).unwrap_or_default()));
                }
            }
            (State::Play, "player_chat") => {
                let body = match v.get("unsignedChatContent") {
                    Some(c) if *c != Value::None => text::plain(c),
                    _ => ["plainMessage", "signedChatContent"]
                        .iter()
                        .find_map(|k| v.get(k).map(text::plain))
                        .unwrap_or_default(),
                };
                let sender = ["networkName", "senderName"].iter().find_map(|k| v.get(k).map(text::plain)).unwrap_or_default();
                let _ = events.try_send(Event::Chat(format!("<{sender}> {body}")));
                self.acknowledge(&v).await?;
            }
            (State::Play, "profileless_chat") => {
                let sender = v.get("name").map(text::plain).unwrap_or_default();
                let body = v.get("message").map(text::plain).unwrap_or_default();
                let _ = events.try_send(Event::Chat(format!("<{sender}> {body}")));
            }
            (State::Play, "set_compression") => {
                self.w.compression_threshold = u32::try_from(v.int("threshold").unwrap_or(-1)).ok();
            }
            _ => {}
        }
        Ok(None)
    }

    /// The absolute position a teleport lands on: each set bit of the low
    /// five `flags` bits (x, y, z, yaw, pitch, in that order in every
    /// protocol that has flags) makes that field relative.
    fn teleport(&mut self, v: &Value) -> [f64; 5] {
        let flags = v.int("flags").unwrap_or(0);
        let prev = self.position.unwrap_or([0.0; 5]);
        let mut at = [0.0; 5];
        for (i, key) in ["x", "y", "z", "yaw", "pitch"].iter().enumerate() {
            let f = v.float(key).unwrap_or(0.0);
            at[i] = if flags & (1 << i) != 0 { prev[i] + f } else { f };
        }
        self.position = Some(at);
        at
    }

    /// Announce the chat session once the player is in the world and the
    /// certificate has arrived, whichever comes last.
    async fn start_chat_session(&mut self) -> Result<(), Error> {
        if self.session_sent || !self.online || self.state != State::Play {
            return Ok(());
        }
        let Some(update) = self.signer.as_ref().map(Signer::session_update) else {
            return Ok(());
        };
        self.session_sent = true;
        self.send(State::Play, "chat_session_update", &update).await
    }

    /// From 1.19.3 the server disconnects a client that leaves too many
    /// signed messages unacknowledged; acknowledge them in batches like the
    /// vanilla client does.
    async fn acknowledge(&mut self, v: &Value) -> Result<(), Error> {
        let signed = matches!(v.get("signature"), Some(Value::Bytes(_)));
        if !signed || self.d.protocol < 761 {
            return Ok(());
        }
        self.unacked += 1;
        if self.unacked >= 64 {
            let count = std::mem::take(&mut self.unacked);
            self.send(State::Play, "message_acknowledgement", &fields! { "count" => count }).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Cookies, chat_text, signable_arguments};
    use crate::protodef::Value;

    #[test]
    fn only_vanilla_message_arguments_are_signed() {
        assert_eq!(signable_arguments("msg Steve hi there"), vec![("message", "hi there".to_owned())]);
        assert_eq!(signable_arguments("minecraft:me waves"), vec![("action", "waves".to_owned())]);
        assert_eq!(signable_arguments("say"), Vec::new());
        assert_eq!(signable_arguments("msg Steve"), Vec::new());
        assert_eq!(signable_arguments("afk"), Vec::new());
    }

    #[test]
    fn cookies_answer_their_key_and_refuse_oversize_values() {
        let mut c = Cookies::default();
        c.store("a:b".into(), vec![1, 2, 3]);
        c.store("a:b".into(), vec![4]);
        c.store("big".into(), vec![0; 6 * 1024]);
        let ask = |k: &str| Value::Map(vec![("cookie".into(), Value::Str(k.into()))]);
        assert_eq!(c.response(&ask("a:b")).get("value"), Some(&Value::Bytes(vec![4])));
        assert_eq!(c.response(&ask("big")).get("value"), Some(&Value::None));
        assert_eq!(c.response(&ask("none")).get("value"), Some(&Value::None));
    }

    #[test]
    fn chat_text_drops_what_servers_reject_and_counts_utf16() {
        assert_eq!(chat_text("a\u{a7}cb\u{7}c\u{7f}", 256), "acbc");
        assert_eq!(chat_text(&"x".repeat(300), 256).len(), 256);
        // each emoji is two UTF-16 units
        assert_eq!(chat_text(&"\u{1f600}".repeat(200), 256).chars().count(), 128);
    }
}
