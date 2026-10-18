use std::io;

use mineflayer_engine::{
    auth::game_profile::GameProfile, core::game_type::GameMode, player::PlayerInfo,
    protocol::connect::ConnectionError,
};

use super::*;

/// The direct translation plus the lifecycle and chat events the
/// bridge derives from a packet (no misc or domain observers).
fn translate(
    state: &mut BridgeState,
    quit_reason: &Mutex<Option<String>>,
    client: &mineflayer_engine::Client,
    event: Event,
) -> Vec<BotEvent> {
    let mut out = super::translate(state, quit_reason, client, event.clone());
    out.extend(packet_events(state, &event));
    out
}

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
    let decode_failed = Event::PacketDecodeFailed {
        packet_id: Some(0x6c),
        error: "boom".to_string(),
    };
    assert!(matches!(
        &translate(&mut s, &qr, &client, decode_failed)[..],
        [BotEvent::Error(e)] if e.contains("id 0x6c") && e.contains("boom")
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
        [BotEvent::Kicked { reason: k, logged_in: false }, BotEvent::End(r)] if k == "banned" && r == "banned"
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
        [BotEvent::Kicked { reason: k, logged_in: true }, BotEvent::End(r)] if k == "banned" && r == "banned"
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
/// but still emits what is derived from them: a respawn packet
/// still yields `Respawn`.
#[tokio::test]
async fn packet_events_off_keeps_derived_events() {
    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let shared = crate::bot::BotShared::new(events_tx);
    let mut rx = shared.events.subscribe();
    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_alive_tx, alive_rx) = watch::channel(());
    let root = tokio_util::sync::CancellationToken::new();
    spawn(
        Bridge::new(
            despawned(),
            shared,
            root.clone(),
            BridgeOptions {
                client_information: None,
                brand: None,
                packet_events: false,
            },
        ),
        input_rx,
        std::time::Duration::from_secs(30),
        alive_rx,
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
    let root = tokio_util::sync::CancellationToken::new();
    spawn(
        Bridge::new(
            despawned(),
            shared,
            root.clone(),
            BridgeOptions {
                client_information: None,
                brand: None,
                packet_events: true,
            },
        ),
        input_rx,
        std::time::Duration::from_secs(30),
        alive_rx,
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
    root.cancelled().await;
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

/// F3: pin the bridge's cross-domain emission order, which `OBSERVERS`
/// states. Within one engine event the direct translation comes first,
/// then misc and scoreboard, then the lifecycle and chat events derived
/// from the packet, then the other domains (a `SetHealth` packet:
/// `Packet`, `Health`, then the re-arm `Spawn`; a chest `BlockEvent`'s
/// `ChestLidMove` from the world observer comes last). Upstream
/// `ref/mineflayer/lib/plugins/health.js` emits `health` before the
/// re-arm `spawn`.
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
    let root = tokio_util::sync::CancellationToken::new();
    spawn(
        Bridge::new(
            client,
            shared,
            root.clone(),
            BridgeOptions {
                client_information: None,
                brand: None,
                packet_events: true,
            },
        ),
        input_rx,
        std::time::Duration::from_secs(30),
        alive_rx,
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
        "ChestLidMove", // world observer, last in the list
        "End",          // fallback on channel close
    ];
    assert_eq!(labels, expected, "emission order changed: {seen:#?}");
}
