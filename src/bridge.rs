//! The engine [`Event`] → [`BotEvent`] translation task.
//!
//! Spawned once per `Bot` by [`BotBuilder::connect`]. This is the pattern
//! every later event source follows: take an engine event stream, translate
//! to `BotEvent`s, forward over the broadcast channel, and shut the client
//! down when every `Bot` handle is gone.

use std::{collections::HashSet, sync::Arc};

use mineflayer_engine::{
    ClientInformation, Event,
    client_chat::ChatPacket,
    player::GameProfileComponent,
    protocol::packets::game::{ClientboundGamePacket, c_system_chat::ClientboundSystemChat},
};
use tokio::sync::{mpsc::UnboundedReceiver, watch};
use tracing::debug;

use parking_lot::Mutex;

use crate::{
    BotEvent,
    bot::with_component_mut,
    chat::{ChatState, TabList},
    entity,
};

/// Connection policy the bridge needs beyond the shared domain state —
/// deliberately small: domains live in [`crate::bot::BotShared`].
pub(crate) struct BridgeOptions {
    /// `options.*` client-information fields from the builder, sent as
    /// built via `set_client_information` on `Event::Init` so the login
    /// packet carries them. `None` sends nothing, leaving the engine's
    /// defaults on the wire.
    pub client_information: Option<ClientInformation>,
    /// `options.brand` — sent as the `minecraft:brand` custom payload on
    /// `Event::Login`; overwrites the `"vanilla"` the engine already sent during
    /// the config phase (the server keeps the last brand received).
    pub brand: Option<String>,
    /// `BotBuilder::packet_events` — `false` keeps `BotEvent::Packet` off
    /// the broadcast; the misc decode still sees every packet.
    pub packet_events: bool,
}

/// Spawn the bridge task. The task ends — and calls `client.exit()` — when
/// The engine's event channel closes (disconnect/shutdown) or when the last
/// `Bot` handle drops (all `alive` senders gone → `changed()` errors).
/// The task's end cancels the bot's root token, which retires the
/// tick-driven driver tasks whose `TickBroadcast` sender outlives the
/// connection.
///
/// `check_timeout_interval` is `options.checkTimeoutInterval`: the server
/// keepalive watchdog, armed on every `Event::KeepAlive` like upstream's
/// per-keepalive `setTimeout` re-arm.
pub(crate) fn spawn(
    mut bridge: Bridge,
    mut rx: UnboundedReceiver<Event>,
    check_timeout_interval: std::time::Duration,
    mut alive: watch::Receiver<()>,
) {
    tokio::spawn(async move {
        // Mark the initial watch value as seen so `changed()` below only
        // resolves when the channel closes (all `Bot`s dropped).
        let _ = alive.borrow_and_update();
        // Keepalive watchdog deadline (`options.checkTimeoutInterval`) —
        // `None` until the first keepalive arms it, like upstream's
        // per-keepalive `setTimeout`.
        let mut keepalive_deadline = None;
        let exit_reason = loop {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else {
                        break "engine event channel closed";
                    };
                    if matches!(event, Event::KeepAlive(_)) {
                        keepalive_deadline =
                            Some(tokio::time::Instant::now() + check_timeout_interval);
                    }
                    bridge.handle(event);
                }
                // Server keepalive watchdog (`options.checkTimeoutInterval`):
                // upstream's keepalive.js emits 'error' then ends the client
                // with 'keepAliveError'. The End reason flows through the
                // quit_reason slot that `translate` reads on Disconnect.
                _ = async {
                    match keepalive_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    keepalive_deadline = None;
                    debug!(
                        timeout_ms = check_timeout_interval.as_millis(),
                        "server keepalive timed out — ending with keepAliveError"
                    );
                    let _ = bridge.shared.events.send(BotEvent::Error(format!(
                        "client timed out after {} milliseconds",
                        check_timeout_interval.as_millis()
                    )));
                    *bridge.shared.quit_reason.lock() = Some("keepAliveError".to_string());
                    bridge.client.disconnect();
                }
                _ = alive.changed() => break "all Bot handles dropped",
            }
        };
        debug!(
            reason = exit_reason,
            ended = bridge.state.ended,
            "bridge exit"
        );
        exit(
            &mut bridge.state,
            &bridge.shared.quit_reason,
            &bridge.shared.events,
            &bridge.root,
            &bridge.client,
        );
    });
}

/// The bridge's state and the per-event work: run every observer on an
/// engine event and send the `BotEvent`s they derive.
pub(crate) struct Bridge {
    client: mineflayer_engine::Client,
    shared: crate::bot::BotShared,
    root: tokio_util::sync::CancellationToken,
    client_information: Option<ClientInformation>,
    brand: Option<String>,
    packet_events: bool,
    state: BridgeState,
}

impl Bridge {
    pub(crate) fn new(
        client: mineflayer_engine::Client,
        shared: crate::bot::BotShared,
        root: tokio_util::sync::CancellationToken,
        options: BridgeOptions,
    ) -> Self {
        let state = BridgeState::default();
        Self {
            client,
            shared,
            root,
            client_information: options.client_information,
            brand: options.brand,
            packet_events: options.packet_events,
            state,
        }
    }

    pub(crate) fn handle(&mut self, event: Event) {
        // Client information must be set on Init so the login packet
        // already carries it (the engine docs).
        if let Event::Init = &event
            && let Some(info) = &self.client_information
        {
            self.client.set_client_information(info.clone());
        }
        // `options.brand` — upstream writes it on the login packet; ours
        // overwrites the engine's config-phase "vanilla".
        if let (Event::Login, Some(brand)) = (&event, &self.brand) {
            use mineflayer_engine::buf::McBuf;
            let mut data = Vec::new();
            if let Err(error) = brand.clone().mc_write(&mut data) {
                debug!(?error, "brand payload failed to encode — sending none");
            }
            self.client.write_packet(
                mineflayer_engine::protocol::packets::game::s_custom_payload::ServerboundCustomPayload {
                    identifier: "brand".into(),
                    data: data.into(),
                },
            );
        }
        // every observer sees the event before any of its events go out,
        // so a subscriber that reacts to one reads domain state that
        // already reflects the event
        let mut cx = Observe {
            client: &self.client,
            shared: &self.shared,
            local: &mut self.state,
        };
        let derived: Vec<BotEvent> = OBSERVERS
            .iter()
            .flat_map(|observe| observe(&mut cx, &event))
            .collect();
        for bot_event in derived {
            // upstream chat.js keeps `endReason` for the pre-play error
            // message — record it before the event goes out so a racing
            // `chat()` sees it.
            if let BotEvent::End(reason) = &bot_event {
                *self.shared.end_reason.lock() = Some(reason.clone());
            }
            // SendError = no subscribers right now; that's fine, receivers
            // may subscribe later.
            if self.packet_events || !matches!(bot_event, BotEvent::Packet(_)) {
                let _ = self.shared.events.send(bot_event);
            }
        }
    }
}

/// The engine stopped feeding events or all `Bot` handles are gone — shut
/// the client down so the ECS runner doesn't leak. Cancelling the root
/// token retires driver tasks parked on `ticks.recv()` — that broadcast's
/// sender lives in the World, which their own `Client` handle keeps
/// alive, so the channel never closes on its own.
///
/// The channel can close without a `Disconnect`/`ConnectionFailed` event
/// (the engine's runner just stops), so emit the same `End` a local close
/// would ("socketClosed", or the stored quit reason) before retiring the
/// drivers — an `end` wait must never hang on a signal that never comes.
fn exit(
    state: &mut BridgeState,
    quit_reason: &Mutex<Option<String>>,
    events: &tokio::sync::broadcast::Sender<BotEvent>,
    root: &tokio_util::sync::CancellationToken,
    client: &mineflayer_engine::Client,
) {
    if !state.ended {
        state.ended = true;
        let reason = quit_reason
            .lock()
            .take()
            .unwrap_or_else(|| "socketClosed".to_string());
        let _ = events.send(BotEvent::End(reason));
    }
    root.cancel();
    client.exit();
}

/// What every observer in [`OBSERVERS`] gets for one engine event.
struct Observe<'a> {
    client: &'a mineflayer_engine::Client,
    shared: &'a crate::bot::BotShared,
    local: &'a mut BridgeState,
}

type Observer = fn(&mut Observe<'_>, &Event) -> Vec<BotEvent>;

/// The observers, in emission order (D-bridge, pinned by
/// `bridge_emission_order`): the direct translation first (`Packet`,
/// `Tick`, lifecycle), then misc and scoreboard, which derive from that
/// `Packet` or `Tick`, then the lifecycle and chat events derived from the
/// packet, then the other domains. Upstream `health.js` emits `health`
/// before the re-arm `spawn`, which this order keeps.
const OBSERVERS: &[Observer] = &[
    |cx, e| translate(cx.local, &cx.shared.quit_reason, cx.client, e.clone()),
    |cx, e| crate::misc::observe(cx.client, &mut cx.local.misc, e),
    |cx, e| {
        with_component_mut::<crate::scoreboard::ScoreboardState, _>(cx.client, |s| {
            crate::scoreboard::observe(s, e)
        })
        .unwrap_or_default()
    },
    |cx, e| packet_events(cx.client, cx.local, e),
    |cx, e| crate::movement::bridge_events(cx.client, &mut cx.local.movement, e),
    |cx, e| crate::pathfinder::bridge_events(cx.client, e),
    |cx, e| entity::observe_event(cx.client, e),
    |cx, e| crate::windows::observe_event(cx.client, &cx.shared.events, e),
    |cx, e| {
        with_component_mut::<crate::window_types::WindowDataTracker, _>(cx.client, |t| {
            crate::window_types::observe_event(t, e)
        })
        .unwrap_or_default()
    },
    |cx, e| {
        with_component_mut::<crate::craft::RecipeBook, _>(cx.client, |b| {
            crate::craft::observe_event(b, e)
        });
        Vec::new()
    },
    |cx, e| crate::world::observe_event(cx.client, &mut cx.local.lids, e),
];

/// Bridge bookkeeping that doesn't fit in the events themselves.
#[derive(Default)]
struct BridgeState {
    /// Whether `Spawn` has been emitted — the engine (0.16) fires `Event::Spawn`
    /// exactly once per entity lifetime (`SentSpawnEvent` is never
    /// removed — verified against events.rs and a live `Simulation`), so
    /// this only dedupes the initial emit. Post-respawn `spawn`s come
    /// from the `c_set_health` packet arm like upstream.
    seen_spawn: bool,
    /// Whether `End` has been emitted — dedupes disconnect signals so `end`
    /// fires exactly once, like upstream.
    ended: bool,
    /// Whether `Connected` has been emitted — it precedes the *first*
    /// `Login` only (upstream `connect` is once per socket; the engine
    /// `Event::Login` also fires exactly once per entity lifetime, so the
    /// gate is belt-and-braces).
    connected: bool,
    /// Upstream's `!isAlive`: set by `Event::Death` (the engine raises it from
    /// two sources for one vanilla death — `player_combat_kill` and
    /// `Changed<Health> == 0`) and by `c_respawn`, cleared by
    /// `c_set_health` with health > 0 (health.js's exact re-arm).
    dead: bool,
    /// Tab-list membership, upstream's `bot.players` mirror. Gates
    /// `playerJoined` (first add only — a re-add is `playerUpdated`),
    /// `playerUpdated` (dropped for never-added players), and `playerLeft`
    /// (dropped for unknown players and for the bot itself). Cleared on
    /// the `c_login` packet like upstream's `bot.players = {}`.
    players: HashSet<uuid::Uuid>,
    /// World-domain chest-lid dedup (`openCountByPos`) — see
    /// [`crate::world::LidCounts`].
    lids: crate::world::LidCounts,
    /// Movement tracking for `move`/`mount`/`dismount`.
    movement: crate::movement::MovementTracker,
    /// The misc domain's per-tick `Sleep`/`Wake`/`Breath` baselines.
    misc: crate::misc::TickBaselines,
}

/// Translate one engine event into zero or more `BotEvent`s, in emission
/// order.
fn translate(
    state: &mut BridgeState,
    quit_reason: &Mutex<Option<String>>,
    client: &mineflayer_engine::Client,
    event: Event,
) -> Vec<BotEvent> {
    match event {
        Event::Init => vec![BotEvent::Init],
        Event::Login
        | Event::Spawn
        | Event::Death(_)
        | Event::Disconnect(_)
        | Event::ConnectionFailed(_) => lifecycle_events(state, quit_reason, event),
        Event::AddPlayer(_) | Event::UpdatePlayer(_) | Event::RemovePlayer(_) => {
            player_list_events(state, client, event)
        }
        Event::Chat(packet) => {
            let mut out = vec![BotEvent::Chat(packet.clone())];
            out.extend(
                with_component_mut::<ChatState, _>(client, |c| c.message_events(&packet))
                    .unwrap_or_default(),
            );
            out
        }
        Event::Tick => vec![BotEvent::Tick],
        Event::Packet(packet) => vec![BotEvent::Packet(packet)],
        Event::ReceiveChunk(pos) => vec![BotEvent::ChunkColumnLoad(pos)],
        // KeepAlive has no mineflayer analog — the engine answers keepalives
        // itself. Dropped by design (see api-parity.md `options.keepAlive`).
        Event::KeepAlive(_) => vec![],
        // the pathfinder observer translates these
        Event::Pathfinder(_) => vec![],
        // the misc observer translates these
        Event::ResourcePack(_) => vec![],
        Event::PacketDecodeFailed { packet_id, error } => {
            let id = packet_id.map_or("an unknown id".to_string(), |id| format!("id {id:#04x}"));
            vec![BotEvent::Error(format!(
                "dropped a game packet ({id}) that did not decode: {error}"
            ))]
        }
    }
}

/// The connection lifecycle: `connect`/`login`, the first `spawn`, `death`
/// once per life, and one `end` (with `kicked` or `error` first).
fn lifecycle_events(
    state: &mut BridgeState,
    quit_reason: &Mutex<Option<String>>,
    event: Event,
) -> Vec<BotEvent> {
    match event {
        Event::Login => {
            // upstream order: 'connect' (socket up) then 'login'. The engine has
            // no socket-level event; Event::Login is the earliest
            // connection-live signal, so Connected rides ahead of it once.
            // (The bot.players resync lives in the packet arm — Event::Login
            // never refires in the engine (0.16).)
            if state.connected {
                vec![BotEvent::Login]
            } else {
                state.connected = true;
                vec![BotEvent::Connected, BotEvent::Login]
            }
        }
        Event::Spawn => {
            if state.seen_spawn {
                vec![]
            } else {
                state.seen_spawn = true;
                vec![BotEvent::Spawn]
            }
        }
        Event::Death(_) => {
            if state.dead {
                return vec![];
            }
            state.dead = true;
            vec![BotEvent::Death]
        }
        Event::Disconnect(reason) => {
            if state.ended {
                return vec![];
            }
            state.ended = true;
            match reason {
                // The engine attaches a reason iff the server kicked us
                // (`DisconnectEvent` docs); `None` means local/socket close.
                Some(reason) => {
                    let reason = reason.to_string();
                    // upstream `kick.js` distinguishes its two listeners by
                    // the kicked phase: an in-game kick reports `loggedIn`
                    // true, a login-phase disconnect false. Login is exactly
                    // the state that `connected` tracks.
                    let logged_in = state.connected;
                    vec![
                        BotEvent::Kicked {
                            reason: reason.clone(),
                            logged_in,
                        },
                        BotEvent::End(reason),
                    ]
                }
                None => {
                    let reason = quit_reason
                        .lock()
                        .take()
                        .unwrap_or_else(|| "socketClosed".to_string());
                    vec![BotEvent::End(reason)]
                }
            }
        }
        Event::ConnectionFailed(error) => {
            if state.ended {
                return vec![];
            }
            state.ended = true;
            // upstream emits 'error' then 'end' on a failed connect
            vec![
                BotEvent::Error(error.to_string()),
                BotEvent::End("socketClosed".to_string()),
            ]
        }
        _ => vec![],
    }
}

/// `playerJoined`/`playerUpdated`/`playerLeft`, gated on upstream's
/// `bot.players` membership.
fn player_list_events(
    state: &mut BridgeState,
    client: &mineflayer_engine::Client,
    event: Event,
) -> Vec<BotEvent> {
    match event {
        Event::AddPlayer(info) => {
            // upstream keys on whether the uuid was already in bot.players:
            // a re-add (the engine emits AddPlayerEvent unconditionally on the
            // add_player action) is `playerUpdated`, not `playerJoined`.
            if state.players.insert(info.uuid) {
                vec![BotEvent::PlayerJoined(info)]
            } else {
                vec![BotEvent::PlayerUpdated(info)]
            }
        }
        Event::UpdatePlayer(info) => {
            // upstream drops info-update actions for players it never added
            if state.players.contains(&info.uuid) {
                vec![BotEvent::PlayerUpdated(info)]
            } else {
                vec![]
            }
        }
        Event::RemovePlayer(info) => {
            // upstream's `continue` on `player.entity === bot.entity` runs
            // before `delete bot.players[uuid]` — check self first so our
            // own remove can't evict us (a later re-add must emit
            // playerUpdated, not playerJoined).
            let self_uuid = client
                .get_component::<GameProfileComponent>()
                .map(|profile| profile.uuid);
            if Some(info.uuid) == self_uuid {
                return vec![];
            }
            // drops removes for players never added
            if !state.players.remove(&info.uuid) {
                return vec![];
            }
            vec![BotEvent::PlayerLeft(info)]
        }
        _ => vec![],
    }
}

/// The lifecycle and chat events derived from a packet. They go out after
/// the misc and scoreboard events of the same packet.
fn packet_events(
    client: &mineflayer_engine::Client,
    state: &mut BridgeState,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    let mut out = lifecycle_packet_events(state, packet);
    out.extend(packet_side_events(client, packet));
    out
}

fn lifecycle_packet_events(
    state: &mut BridgeState,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        // upstream clears `bot.players = {}` on the login packet — a
        // mid-session relogin repopulates the tab list from scratch
        ClientboundGamePacket::Login(_) => {
            state.players.clear();
            vec![]
        }
        // upstream sets isAlive=false and emits 'respawn' on every
        // respawn packet — death respawn and dimension change alike
        ClientboundGamePacket::Respawn(_) => {
            state.dead = true;
            vec![BotEvent::Respawn]
        }
        // upstream's isAlive re-arm: the first set_health with health > 0
        // while dead emits 'spawn' — this carries every post-respawn
        // spawn, since the engine's Event::Spawn only ever fires once
        ClientboundGamePacket::SetHealth(p) if p.health > 0.0 && state.dead => {
            state.dead = false;
            vec![BotEvent::Spawn]
        }
        _ => vec![],
    }
}

/// Extra `BotEvent`s decoded from raw clientbound packets the engine's own
/// handlers drop (chat domain): the dedicated action-bar packet feeds the
/// same message family as a chat packet, `c_tab_list` updates the stored
/// header/footer, and `c_command_suggestions` resolves a `tab_complete` call.
/// The raw packet itself still goes out as `BotEvent::Packet` first.
fn packet_side_events(
    client: &mineflayer_engine::Client,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::SetActionBarText(p) => {
            // upstream routes game_info text through
            // message/messagestr/actionBar, so synthesize the overlay chat
            // packet it would have arrived as on older versions.
            let chat_packet = ChatPacket::System(Arc::new(ClientboundSystemChat {
                content: p.text.clone(),
                overlay: true,
            }));
            with_component_mut::<ChatState, _>(client, |c| c.message_events(&chat_packet))
                .unwrap_or_default()
        }
        ClientboundGamePacket::TabList(p) => {
            with_component_mut::<ChatState, _>(client, |c| {
                c.tablist = TabList {
                    header: p.header.clone(),
                    footer: p.footer.clone(),
                }
            });
            vec![]
        }
        ClientboundGamePacket::CommandSuggestions(p) => {
            let matches = p.suggestions.list().iter().map(|s| s.text()).collect();
            with_component_mut::<ChatState, _>(client, |c| c.complete_suggestions(p.id, matches));
            vec![]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests;
