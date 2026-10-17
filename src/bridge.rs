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
    chat::{ChatShared, TabList},
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
    /// `options.checkTimeoutInterval` — the server keepalive watchdog
    /// interval, armed on every `Event::KeepAlive` like upstream's per-
    /// keepalive `setTimeout` re-arm.
    pub check_timeout_interval: std::time::Duration,
    /// `BotBuilder::packet_events` — `false` keeps `BotEvent::Packet` off
    /// the broadcast; the misc decode still sees every packet.
    pub packet_events: bool,
}

/// Spawn the bridge task. The task ends — and calls `client.exit()` — when
/// The engine's event channel closes (disconnect/shutdown) or when the last
/// `Bot` handle drops (all `alive` senders gone → `changed()` errors).
/// `shutdown` is held for its drop: the task's end resolves `changed()`
/// on every `Bot::shutdown` receiver, retiring the tick-driven driver
/// tasks whose `TickBroadcast` sender outlives the connection.
pub(crate) fn spawn(
    client: mineflayer_engine::Client,
    mut rx: UnboundedReceiver<Event>,
    shared: crate::bot::BotShared,
    options: BridgeOptions,
    mut alive: watch::Receiver<()>,
    shutdown: watch::Sender<()>,
) {
    tokio::spawn(async move {
        let crate::bot::BotShared {
            events,
            quit_reason,
            end_reason,
            chat,
            world_state: world,
            entities,
            windows,
            window_data,
            recipes,
            scoreboard,
        } = shared;
        let BridgeOptions {
            client_information,
            brand,
            check_timeout_interval,
            packet_events,
        } = options;
        // Mark the initial watch value as seen so `changed()` below only
        // resolves when the channel closes (all `Bot`s dropped).
        let _ = alive.borrow_and_update();
        let mut state = BridgeState {
            chat,
            ..Default::default()
        };
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
                    // Client information must be set on Init so the login
                    // packet already carries it (the engine docs).
                    if let Event::Init = &event
                        && let Some(info) = &client_information
                    {
                        client.set_client_information(info.clone());
                    }
                    // `options.brand` — upstream writes it on the login
                    // packet; ours overwrites the engine's config-phase
                    // "vanilla".
                    if let (Event::Login, Some(brand)) = (&event, &brand) {
                        use mineflayer_engine::buf::McBuf;
                        let mut data = Vec::new();
                        if let Err(error) = brand.clone().mc_write(&mut data) {
                            debug!(?error, "brand payload failed to encode — sending none");
                        }
                        client.write_packet(
                            mineflayer_engine::protocol::packets::game::s_custom_payload::ServerboundCustomPayload {
                                identifier: "brand".into(),
                                data: data.into(),
                            },
                        );
                    }
                    let keepalive_seen = matches!(&event, Event::KeepAlive(_));
                    // Domain decodes the engine doesn't lift into `Event`s
                    // (movement: move diff/forcedMove/abilities; entities:
                    // the whole entity event family). Both run before
                    // translate borrows the event; their events are emitted
                    // right after the raw `Packet`/`Spawn` they came from.
                    let movement_events =
                        crate::movement::bridge_events(&client, &mut state.movement, &event);
                    let pathfinder_events = crate::pathfinder::bridge_events(
                        &client,
                        &mut state.pathfinder,
                        &event,
                    );
                    let derived = entity::observe_event(&entities, &client, &event);
                    // window domain: open/close/slot/held bookkeeping and
                    // the ECS patches for pieces the engine drops (its
                    // observer must run *inside* this event loop so ECS
                    // writes land between ticks).
                    let window_events =
                        crate::windows::observe_event(&windows, &client, &event);
                    // typed windows: container_set_data / merchant_offers
                    // decode (furnace/enchant/anvil properties, villager
                    // trades). No ECS access — runs on packet fields only.
                    let window_data_events =
                        crate::window_types::observe_event(&window_data, &event);
                    // recipe book: records c_recipe_book_add/remove into
                    // shared state; emits no events (craft has none)
                    crate::craft::observe_event(&recipes, &event);
                    // world domain: blocks/time/weather/game decode off the
                    // packets the engine drops. Runs here — not in a second task
                    // — so derived events keep packet order and a lagging
                    // broadcast consumer can't drop them.
                    let world_events =
                        crate::world::observe_event(&world, &client, &mut state.lids, &event);
                    for bot_event in translate(&mut state, &quit_reason, &client, event) {
                        // upstream chat.js keeps `endReason` for the
                        // pre-play error message — record it before the
                        // event goes out so a racing `chat()` sees it.
                        if let BotEvent::End(reason) = &bot_event {
                            *end_reason.lock() = Some(reason.clone());
                        }
                        // SendError = no subscribers right now; that's fine,
                        // receivers may subscribe later.
                        if packet_events || !matches!(bot_event, BotEvent::Packet(_)) {
                            let _ = events.send(bot_event.clone());
                        }
                        // misc domains (scoreboards, sounds, …) decode Packet
                        // events into facade state + their own BotEvents,
                        // emitted after the source event itself.
                        crate::misc::after_event(&client, &scoreboard, &bot_event, &events);
                    }
                    for bot_event in movement_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in pathfinder_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in derived {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in window_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in window_data_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in world_events {
                        let _ = events.send(bot_event);
                    }
                    if keepalive_seen {
                        keepalive_deadline =
                            Some(tokio::time::Instant::now() + check_timeout_interval);
                    }
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
                    let _ = events.send(BotEvent::Error(format!(
                        "client timed out after {} milliseconds",
                        check_timeout_interval.as_millis()
                    )));
                    *quit_reason.lock() = Some("keepAliveError".to_string());
                    client.disconnect();
                }
                _ = alive.changed() => break "all Bot handles dropped",
            }
        };
        debug!(reason = exit_reason, ended = state.ended, "bridge exit");
        exit(&mut state, &quit_reason, &events, shutdown, &client);
    });
}

/// The engine stopped feeding events or all `Bot` handles are gone — shut
/// the client down so the ECS runner doesn't leak. Dropping the shutdown
/// sender retires driver tasks parked on `ticks.recv()` — that broadcast's
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
    shutdown: watch::Sender<()>,
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
    drop(shutdown);
    client.exit();
}

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
    /// Movement tracking for `move`/`mount`/`dismount` (U5).
    movement: crate::movement::MovementTracker,
    /// Chat-domain state shared with `Bot` (pattern registry, tablist,
    /// `tab_complete` correlation).
    chat: Arc<Mutex<ChatShared>>,
    /// Pathfinder component-diff state for the goal/path events (U7).
    pathfinder: crate::pathfinder::PathTracker,
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
                        BotEvent::Kicked(reason.clone(), logged_in),
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
        Event::Chat(packet) => {
            let mut out = vec![BotEvent::Chat(packet.clone())];
            out.extend(state.chat.lock().message_events(&packet));
            out
        }
        Event::Tick => vec![BotEvent::Tick],
        Event::Packet(packet) => {
            let mut out = vec![BotEvent::Packet(packet.clone())];
            out.extend(lifecycle_packet_events(state, &packet));
            out.extend(packet_side_events(&state.chat, &packet));
            out
        }
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
        Event::ReceiveChunk(pos) => vec![BotEvent::ChunkColumnLoad(pos)],
        // KeepAlive has no mineflayer analog — the engine answers keepalives
        // itself. Dropped by design (see api-parity.md `options.keepAlive`).
        Event::KeepAlive(_) => vec![],
        // The engine's Event is #[non_exhaustive]: anything it adds upstream in
        // the future is dropped rather than crashing the bridge — with a
        // debug log so a new upstream event does not vanish silently.
        _ => {
            debug!(
                ?event,
                "dropping an engine event the bridge does not translate"
            );
            vec![]
        }
    }
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
fn packet_side_events(chat: &Mutex<ChatShared>, packet: &ClientboundGamePacket) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::SetActionBarText(p) => {
            // upstream routes game_info text through
            // message/messagestr/actionBar, so synthesize the overlay chat
            // packet it would have arrived as on older versions.
            let chat_packet = ChatPacket::System(Arc::new(ClientboundSystemChat {
                content: p.text.clone(),
                overlay: true,
            }));
            chat.lock().message_events(&chat_packet)
        }
        ClientboundGamePacket::TabList(p) => {
            chat.lock().tablist = TabList {
                header: p.header.clone(),
                footer: p.footer.clone(),
            };
            vec![]
        }
        ClientboundGamePacket::CommandSuggestions(p) => {
            let matches = p.suggestions.list().iter().map(|s| s.text()).collect();
            chat.lock().complete_suggestions(p.id, matches);
            vec![]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use mineflayer_engine::{
        auth::game_profile::GameProfile, core::game_type::GameMode, player::PlayerInfo,
        protocol::connect::ConnectionError,
    };

    use super::*;

    fn despawned() -> mineflayer_engine::Client {
        crate::test_util::despawned_client()
    }

    fn player_info(uuid: uuid::Uuid) -> PlayerInfo {
        PlayerInfo {
            profile: GameProfile::new(uuid, "faye".into()),
            uuid,
            gamemode: GameMode::Survival,
            latency: 0,
            display_name: None,
        }
    }

    fn packet_event(packet: ClientboundGamePacket) -> Event {
        Event::Packet(Arc::new(packet))
    }

    /// Upstream's lifecycle hooks are per-packet (`respawn`, `login`,
    /// `update_health`) — the engine `Event::Spawn`/`Event::Login` fire only
    /// once per entity lifetime, so the tests drive the packet arm the
    /// same way the wire does.
    fn set_health(health: f32) -> Event {
        use mineflayer_engine::protocol::packets::game::c_set_health::ClientboundSetHealth;
        packet_event(ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health,
            food: 20,
            saturation: 5.0,
        }))
    }

    fn spawn_info() -> mineflayer_engine::protocol::packets::common::CommonPlayerSpawnInfo {
        use mineflayer_engine::{
            core::game_type::OptionalGameType,
            registry::{DataRegistry as _, data::DimensionKind},
        };
        mineflayer_engine::protocol::packets::common::CommonPlayerSpawnInfo {
            dimension_type: DimensionKind::new_raw(0),
            dimension: mineflayer_engine::Identifier::new("minecraft:overworld"),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
        }
    }

    fn respawn_packet() -> Event {
        use mineflayer_engine::protocol::packets::game::c_respawn::ClientboundRespawn;
        packet_event(ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: spawn_info(),
            data_to_keep: 0,
        }))
    }

    fn login_packet() -> Event {
        use mineflayer_engine::protocol::packets::game::c_login::ClientboundLogin;
        packet_event(ClientboundGamePacket::Login(ClientboundLogin {
            player_id: mineflayer_engine::core::entity_id::MinecraftEntityId(0),
            hardcore: false,
            levels: vec![],
            max_players: 20,
            chunk_radius: 2,
            simulation_distance: 2,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            common: spawn_info(),
            enforces_secure_chat: false,
        }))
    }

    #[test]
    fn translate_one_to_one() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Init)[..],
            [BotEvent::Init]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Login)[..],
            [BotEvent::Connected, BotEvent::Login]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Tick)[..],
            [BotEvent::Tick]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Death(None))[..],
            [BotEvent::Death]
        ));
        assert!(matches!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::ReceiveChunk(mineflayer_engine::core::position::ChunkPos::new(1, 2))
            )[..],
            [BotEvent::ChunkColumnLoad(_)]
        ));
    }

    #[test]
    fn translate_death_once_per_life() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        // a vanilla death reaches the engine twice — player_combat_kill and the
        // Changed<Health>==0 listener — but upstream's isAlive edge makes
        // `death` strictly once per life
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Death(None))[..],
            [BotEvent::Death]
        ));
        assert!(translate(&mut s, &qr, &client, Event::Death(None)).is_empty());
        assert!(translate(&mut s, &qr, &client, Event::Death(None)).is_empty());
        // the respawn packet emits 'respawn' and keeps isAlive false —
        // upstream suppresses a death that lands before the health update
        assert!(matches!(
            translate(&mut s, &qr, &client, respawn_packet())[..],
            [BotEvent::Packet(_), BotEvent::Respawn]
        ));
        assert!(translate(&mut s, &qr, &client, Event::Death(None)).is_empty());
        // upstream's re-arm is set_health with health > 0 — that emits the
        // post-respawn 'spawn' and re-arms death
        assert!(matches!(
            translate(&mut s, &qr, &client, set_health(20.0))[..],
            [BotEvent::Packet(_), BotEvent::Spawn]
        ));
        // a health packet while alive emits nothing extra
        assert!(matches!(
            translate(&mut s, &qr, &client, set_health(20.0))[..],
            [BotEvent::Packet(_)]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Death(None))[..],
            [BotEvent::Death]
        ));
        assert!(translate(&mut s, &qr, &client, Event::Death(None)).is_empty());
    }

    #[test]
    fn translate_player_join_left_gates() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let uuid = uuid::Uuid::from_u128(0x1234);

        // first add -> playerJoined
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::AddPlayer(player_info(uuid)))[..],
            [BotEvent::PlayerJoined(_)]
        ));
        // a re-add (the engine fires AddPlayerEvent unconditionally) -> playerUpdated
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::AddPlayer(player_info(uuid)))[..],
            [BotEvent::PlayerUpdated(_)]
        ));
        // an update for a never-added player -> nothing (upstream's `!player`)
        let stranger = uuid::Uuid::from_u128(0xdead);
        assert!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::UpdatePlayer(player_info(stranger))
            )
            .is_empty()
        );
        // an update for a listed player -> playerUpdated
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::UpdatePlayer(player_info(uuid)))[..],
            [BotEvent::PlayerUpdated(_)]
        ));
        // remove of a never-added player -> nothing
        assert!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::RemovePlayer(player_info(stranger))
            )
            .is_empty()
        );
        // remove of a listed player -> playerLeft
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::RemovePlayer(player_info(uuid)))[..],
            [BotEvent::PlayerLeft(_)]
        ));
        // upstream clears bot.players on the login packet (Event::Login
        // never refires) — after a relogin the same uuid joins again
        // instead of updating
        assert!(matches!(
            translate(&mut s, &qr, &client, login_packet())[..],
            [BotEvent::Packet(_)]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::AddPlayer(player_info(uuid)))[..],
            [BotEvent::PlayerJoined(_)]
        ));
    }

    #[test]
    fn translate_remove_player_skips_self() {
        let self_uuid = uuid::Uuid::from_u128(0xbeef);
        let client = crate::test_util::live_entity_client(GameProfileComponent(GameProfile::new(
            self_uuid,
            "bot".into(),
        )));
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);

        translate(
            &mut s,
            &qr,
            &client,
            Event::AddPlayer(player_info(self_uuid)),
        );
        // our own tab entry's removal is never playerLeft upstream
        assert!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::RemovePlayer(player_info(self_uuid))
            )
            .is_empty()
        );
        // and it must not evict us from the set — a mid-session re-add of
        // self is playerUpdated upstream, not playerJoined
        assert!(matches!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::AddPlayer(player_info(self_uuid))
            )[..],
            [BotEvent::PlayerUpdated(_)]
        ));
        // other players still leave normally
        let other = uuid::Uuid::from_u128(0x1234);
        translate(&mut s, &qr, &client, Event::AddPlayer(player_info(other)));
        assert!(matches!(
            translate(
                &mut s,
                &qr,
                &client,
                Event::RemovePlayer(player_info(other))
            )[..],
            [BotEvent::PlayerLeft(_)]
        ));
    }

    #[test]
    fn translate_connected_fires_once_before_first_login() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        // upstream 'connect' precedes 'login' and fires once per connection
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Login)[..],
            [BotEvent::Connected, BotEvent::Login]
        ));
        // world switches re-login; connect does not refire
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Login)[..],
            [BotEvent::Login]
        ));
    }

    #[test]
    fn translate_spawn_then_respawn() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        // Event::Spawn fires once per entity lifetime in the engine (0.16) —
        // the dedup guard covers the hypothetical refire
        assert!(matches!(
            translate(&mut s, &qr, &client, Event::Spawn)[..],
            [BotEvent::Spawn]
        ));
        assert!(translate(&mut s, &qr, &client, Event::Spawn).is_empty());
        // respawns (death, dimension change) are packet-driven like
        // upstream: respawn packet -> 'respawn', post-respawn health
        // update -> 'spawn'
        assert!(matches!(
            translate(&mut s, &qr, &client, respawn_packet())[..],
            [BotEvent::Packet(_), BotEvent::Respawn]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, set_health(20.0))[..],
            [BotEvent::Packet(_), BotEvent::Spawn]
        ));
        // a dimension-change respawn while alive produces the same pair
        assert!(matches!(
            translate(&mut s, &qr, &client, respawn_packet())[..],
            [BotEvent::Packet(_), BotEvent::Respawn]
        ));
        assert!(matches!(
            translate(&mut s, &qr, &client, set_health(20.0))[..],
            [BotEvent::Packet(_), BotEvent::Spawn]
        ));
    }

    #[test]
    fn translate_disconnect_variants() {
        let client = despawned();
        let qr = Mutex::new(None);

        // plain close -> End("socketClosed"), upstream's default reason
        let mut s = BridgeState::default();
        assert!(matches!(
            &translate(&mut s, &qr, &client, Event::Disconnect(None))[..],
            [BotEvent::End(r)] if r == "socketClosed"
        ));
        // End only fires once
        assert!(translate(&mut s, &qr, &client, Event::Disconnect(None)).is_empty());

        // kick -> Kicked then End with the kick reason; no login first, so
        // this is upstream's login-phase listener: `loggedIn = false`
        let mut s = BridgeState::default();
        assert!(matches!(
            &translate(
                &mut s,
                &qr,
                &client,
                Event::Disconnect(Some(mineflayer_engine::FormattedText::from("banned")))
            )[..],
            [BotEvent::Kicked(k, false), BotEvent::End(r)] if k == "banned" && r == "banned"
        ));

        // after a login, the in-game handler reports `loggedIn = true`
        let mut s = BridgeState::default();
        assert!(
            translate(&mut s, &qr, &client, Event::Login)
                .iter()
                .any(|e| matches!(e, BotEvent::Login))
        );
        assert!(matches!(
            &translate(
                &mut s,
                &qr,
                &client,
                Event::Disconnect(Some(mineflayer_engine::FormattedText::from("banned")))
            )[..],
            [BotEvent::Kicked(k, true), BotEvent::End(r)] if k == "banned" && r == "banned"
        ));
    }

    #[test]
    fn translate_disconnect_uses_quit_reason() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(Some("disconnect.quitting".to_string()));
        assert!(matches!(
            &translate(&mut s, &qr, &client, Event::Disconnect(None))[..],
            [BotEvent::End(r)] if r == "disconnect.quitting"
        ));
        assert!(qr.lock().is_none());
    }

    #[test]
    fn translate_connection_failed_is_error_then_end() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let err = ConnectionError::from(io::Error::other("refused"));
        assert!(matches!(
            &translate(&mut s, &qr, &client, Event::ConnectionFailed(Arc::new(err)))[..],
            [BotEvent::Error(e), BotEvent::End(r)] if e.contains("refused") && r == "socketClosed"
        ));
    }

    #[test]
    fn translate_keepalive_dropped() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        assert!(translate(&mut s, &qr, &client, Event::KeepAlive(42)).is_empty());
    }

    #[test]
    fn translate_chat_emits_message_family() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let out = translate(
            &mut s,
            &qr,
            &client,
            Event::Chat(mineflayer_engine::client_chat::ChatPacket::new("<a> hi")),
        );
        assert!(
            matches!(&out[..],
                [
                    BotEvent::Chat(_),
                    BotEvent::Message { position: crate::MessagePosition::System, .. },
                    BotEvent::MessageStr { message, .. },
                ] if message == "<a> hi"),
            "expected Chat+Message+MessageStr, got {out:?}"
        );
    }

    #[test]
    fn translate_action_bar_packet_feeds_message_family() {
        use mineflayer_engine::protocol::packets::game::c_set_action_bar_text::ClientboundSetActionBarText;

        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let packet = ClientboundGamePacket::SetActionBarText(ClientboundSetActionBarText {
            text: mineflayer_engine::FormattedText::from("bar!"),
        });
        let out = translate(&mut s, &qr, &client, Event::Packet(Arc::new(packet)));
        assert!(
            matches!(&out[..],
                [
                    BotEvent::Packet(_),
                    BotEvent::Message { position: crate::MessagePosition::GameInfo, .. },
                    BotEvent::MessageStr { message, .. },
                    BotEvent::ActionBar(_),
                ] if message == "bar!"),
            "expected Packet+Message+MessageStr+ActionBar, got {out:?}"
        );
    }

    #[test]
    fn translate_tab_list_packet_updates_shared() {
        use mineflayer_engine::protocol::packets::game::c_tab_list::ClientboundTabList;

        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let packet = ClientboundGamePacket::TabList(ClientboundTabList {
            header: mineflayer_engine::FormattedText::from("h"),
            footer: mineflayer_engine::FormattedText::from("f"),
        });
        let out = translate(&mut s, &qr, &client, Event::Packet(Arc::new(packet)));
        assert!(matches!(&out[..], [BotEvent::Packet(_)]));
        let tablist = s.chat.lock().tablist.clone();
        assert_eq!(tablist.header.to_string(), "h");
        assert_eq!(tablist.footer.to_string(), "f");
    }

    #[test]
    fn translate_command_suggestions_resolves_pending() {
        use mineflayer_engine::{
            brigadier::{
                context::StringRange,
                suggestion::{Suggestion, Suggestions},
            },
            protocol::packets::game::c_command_suggestions::ClientboundCommandSuggestions,
        };

        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let (id, mut rx) = s.chat.lock().register_completion();
        let packet = ClientboundGamePacket::CommandSuggestions(ClientboundCommandSuggestions {
            id,
            suggestions: Suggestions::new(
                StringRange::new(0, 2),
                vec![Suggestion::new(StringRange::new(0, 2), "gamemode")],
            ),
        });
        let out = translate(&mut s, &qr, &client, Event::Packet(Arc::new(packet)));
        assert!(matches!(&out[..], [BotEvent::Packet(_)]));
        assert_eq!(rx.try_recv().unwrap(), vec!["gamemode".to_string()]);
    }

    #[test]
    fn translate_chat_runs_patterns() {
        let client = despawned();
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        s.chat.lock().patterns.add(
            "probe",
            vec![regex::Regex::new("worked").unwrap()],
            crate::ChatPatternOpts::default(),
        );
        let out = translate(
            &mut s,
            &qr,
            &client,
            Event::Chat(mineflayer_engine::client_chat::ChatPacket::new("it worked")),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::ChatPattern { name, .. } if name == "probe")),
            "expected a ChatPattern event, got {out:?}"
        );
    }

    /// `packet_events(false)` keeps raw `Packet` events off the broadcast
    /// but still emits what is derived from them (C2): a respawn packet
    /// still yields `Respawn`.
    #[tokio::test]
    async fn packet_events_off_keeps_derived_events() {
        let (events_tx, _) = tokio::sync::broadcast::channel(64);
        let shared = crate::bot::BotShared::new(events_tx);
        let mut rx = shared.events.subscribe();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_alive_tx, alive_rx) = watch::channel(());
        let (shutdown_tx, _shutdown_rx) = watch::channel(());
        spawn(
            despawned(),
            input_rx,
            shared,
            BridgeOptions {
                client_information: None,
                brand: None,
                check_timeout_interval: std::time::Duration::from_secs(30),
                packet_events: false,
            },
            alive_rx,
            shutdown_tx,
        );
        input_tx.send(respawn_packet()).unwrap();
        drop(input_tx);
        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("the bridge stalled")
                .expect("broadcast channel closed");
            let end = matches!(event, BotEvent::End(_));
            seen.push(event);
            if end {
                break;
            }
        }
        assert!(
            !seen.iter().any(|e| matches!(e, BotEvent::Packet(_))),
            "no raw Packet event may go out: {seen:?}"
        );
        assert!(
            seen.iter().any(|e| matches!(e, BotEvent::Respawn)),
            "the derived Respawn must still go out: {seen:?}"
        );
    }

    /// The engine channel can close without a `Disconnect` event (its
    /// runner stops). The bridge must still emit `End` — upstream's socket
    /// close — or a wait on `End` never resolves.
    #[tokio::test]
    async fn channel_close_without_disconnect_emits_end() {
        let (events_tx, _) = tokio::sync::broadcast::channel(8);
        let shared = crate::bot::BotShared::new(events_tx);
        let mut end_rx = shared.events.subscribe();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_alive_tx, alive_rx) = watch::channel(());
        let (shutdown_tx, mut shutdown_rx) = watch::channel(());
        spawn(
            despawned(),
            input_rx,
            shared,
            BridgeOptions {
                client_information: None,
                brand: None,
                check_timeout_interval: std::time::Duration::from_secs(30),
                packet_events: true,
            },
            alive_rx,
            shutdown_tx,
        );
        // close the engine event channel without a Disconnect event
        drop(input_tx);
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), end_rx.recv())
            .await
            .expect("the bridge closed without emitting End")
            .expect("broadcast channel closed");
        match event {
            BotEvent::End(reason) => assert_eq!(reason, "socketClosed"),
            other => panic!("expected End, got {other:?}"),
        }
        // exactly one End (the bridge owns every sender clone, so the
        // channel may also have closed by now)
        assert!(matches!(
            end_rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
        // and the bridge retired the drivers
        assert!(shutdown_rx.changed().await.is_err());
    }

    fn label(event: &BotEvent) -> String {
        match event {
            BotEvent::Packet(_) => "Packet".to_string(),
            BotEvent::Death => "Death".to_string(),
            BotEvent::Health => "Health".to_string(),
            BotEvent::Spawn => "Spawn".to_string(),
            BotEvent::Respawn => "Respawn".to_string(),
            BotEvent::GameStateChanged => "GameStateChanged".to_string(),
            BotEvent::Message { .. } => "Message".to_string(),
            BotEvent::MessageStr { .. } => "MessageStr".to_string(),
            BotEvent::ActionBar(_) => "ActionBar".to_string(),
            BotEvent::NoteHeard { .. } => "NoteHeard".to_string(),
            BotEvent::ChestLidMove { .. } => "ChestLidMove".to_string(),
            BotEvent::End(_) => "End".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// F3: pin the bridge's cross-domain emission order.
    ///
    /// Only the bridge loop states this order today. Within one engine
    /// event: `translate`'s direct events are sent one by one, each
    /// followed immediately by what `misc::after_event` derives from it
    /// (a `SetHealth` packet: `Packet`, then `Health`, then the lifecycle
    /// `Spawn`); then the held domain lists go out in a fixed order —
    /// movement, pathfinder, entity, windows, window types, world (a
    /// chest `BlockEvent`'s `ChestLidMove` is decoded before `translate`
    /// but sent last). A later refactor (B1) must keep this order or
    /// change it on purpose; upstream `ref/mineflayer/lib/plugins/health.js`
    /// emits `health` before the re-arm `spawn`, which `Packet -> Health ->
    /// Spawn` preserves.
    #[tokio::test]
    async fn bridge_emission_order() {
        use mineflayer_engine::{
            block::BlockState,
            core::position::{BlockPos, ChunkPos},
            local_player::WorldHolder,
            protocol::packets::game::{
                c_block_event::ClientboundBlockEvent,
                c_set_action_bar_text::ClientboundSetActionBarText,
            },
            registry::builtin::BlockKind,
            world::{Chunk, PartialChunkStorage, World},
        };

        // A live entity (the misc decode writes `MiscState`) over a small
        // loaded world (the world decode needs `WorldHolder` + chunks to
        // resolve the chest).
        let client = crate::test_util::live_entity_client(());
        let mut world = World::default();
        let mut partial = PartialChunkStorage::default();
        for x in -1..=1 {
            for z in -1..=1 {
                partial.set(
                    &ChunkPos { x, z },
                    Some(Chunk::default()),
                    &mut world.chunks,
                );
            }
        }
        let chest_pos = BlockPos::new(1, 64, 1);
        world
            .chunks
            .set_block_state(chest_pos, BlockState::from(BlockKind::Chest));
        let shared_world = std::sync::Arc::new(parking_lot::RwLock::new(world));
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(WorldHolder::new(client.entity, shared_world.clone()));

        let (events_tx, _) = tokio::sync::broadcast::channel(64);
        let shared = crate::bot::BotShared::new(events_tx);
        let mut rx = shared.events.subscribe();
        let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_alive_tx, alive_rx) = watch::channel(());
        let (shutdown_tx, _shutdown_rx) = watch::channel(());
        spawn(
            client,
            input_rx,
            shared,
            BridgeOptions {
                client_information: None,
                brand: None,
                check_timeout_interval: std::time::Duration::from_secs(30),
                packet_events: true,
            },
            alive_rx,
            shutdown_tx,
        );

        // A death so the health packet exercises the re-arm path.
        input_tx.send(Event::Death(None)).unwrap();
        // SetHealth above zero while dead: Spawn (lifecycle) + Health (misc).
        input_tx.send(set_health(20.0)).unwrap();
        input_tx.send(respawn_packet()).unwrap();
        input_tx
            .send(packet_event(ClientboundGamePacket::SetActionBarText(
                ClientboundSetActionBarText {
                    text: mineflayer_engine::FormattedText::from("bar!"),
                },
            )))
            .unwrap();
        // A note block (misc NoteHeard) and a chest (world ChestLidMove).
        input_tx
            .send(packet_event(ClientboundGamePacket::BlockEvent(
                ClientboundBlockEvent {
                    pos: BlockPos::new(0, 64, 0),
                    action_id: 0,
                    action_parameter: 8,
                    block: BlockKind::NoteBlock,
                },
            )))
            .unwrap();
        input_tx
            .send(packet_event(ClientboundGamePacket::BlockEvent(
                ClientboundBlockEvent {
                    pos: chest_pos,
                    action_id: 1,
                    action_parameter: 1,
                    block: BlockKind::Chest,
                },
            )))
            .unwrap();
        drop(input_tx);

        let mut seen = Vec::new();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("the bridge stalled")
                .expect("broadcast channel closed");
            let is_end = matches!(event, BotEvent::End(_));
            seen.push(event);
            if is_end {
                break;
            }
        }

        let labels: Vec<String> = seen.iter().map(label).collect();
        let expected = [
            "Death",            // Event::Death
            "Packet",           // set_health
            "Health",           // misc, straight after its Packet
            "Spawn",            // lifecycle re-arm (dead -> alive)
            "Packet",           // respawn
            "Respawn",          // lifecycle
            "GameStateChanged", // world decode of the same respawn
            "Packet",           // action bar
            "Message",          // chat family (packet_side_events)
            "MessageStr",
            "ActionBar",
            "Packet",       // note block
            "NoteHeard",    // misc, straight after its Packet
            "Packet",       // chest block event
            "ChestLidMove", // world list, after the six held lists
            "End",          // fallback on channel close
        ];
        assert_eq!(labels, expected, "emission order changed: {seen:#?}");
    }
}
