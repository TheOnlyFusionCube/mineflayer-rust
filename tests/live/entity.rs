//! Live entity test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u4bot` and exercises the U4
//! surface: the bot's own entity, the tab-list `players()`, `find_players`,
//! `entities()`, `swing_arm`, and the `entitySpawn` event emitted for the
//! bot itself on spawn (upstream semantics). Only compiled with
//! `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1 (single `live` binary)
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::wait_for;
use mineflayer::{
    Bot, BotEvent,
    entity::{EntityExt, PlayerFilter},
};
use mineflayer_engine::{
    core::entity_id::MinecraftEntityId, protocol::packets::game::s_interact::InteractionHand,
};

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn entities() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u4bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();

    wait_for(
        &mut rx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "spawn",
    )
    .await;

    // upstream emits `entitySpawn` for the bot itself right after spawn —
    // it should already be in the stream or arrive immediately
    wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::EntitySpawn(entity) if entity.id() == bot.entity().id()),
        "entitySpawn for own entity",
    )
    .await;

    // the bot's own entity exists in the world
    let own = bot.entity();
    assert!(own.exists(), "own entity should exist");
    assert!(own.is_alive(), "own entity should be alive");
    let own_id = *own
        .get_component::<MinecraftEntityId>()
        .expect("own entity has a minecraft id");

    // `bot.entities` contains our own entity, keyed by protocol id
    let entities = bot.entities();
    assert!(
        entities.contains_key(&own_id),
        "entities() should contain own id {own_id:?}"
    );

    // tab list: `bot.players` contains self, with username/ping/gamemode
    let players = bot.players();
    let me = players
        .get("u4bot")
        .expect("players() should contain u4bot");
    assert_eq!(me.username(), "u4bot");
    assert!(me.ping() >= 0);
    // the server spawned us, so our player has a world entity handle
    assert!(me.entity.is_some(), "own player should have an entity");

    // `bot.player` resolves to ourselves; `uuidToUsername` maps it back
    let me = bot.player().expect("player() should find u4bot");
    assert_eq!(me.username(), "u4bot");
    let uuids = bot.uuid_to_username();
    assert_eq!(
        uuids.get(&me.info.uuid.to_string()),
        Some(&"u4bot".to_string())
    );

    // `find_players` by username finds our own player entity (upstream
    // `findPlayer` also matches the bot itself)
    let found = bot.find_players(PlayerFilter::Username("u4bot"));
    assert!(
        found.iter().any(|e| e.id() == own.id()),
        "find_players should find own entity"
    );
    assert!(
        bot.find_players(PlayerFilter::Username("nobody-online"))
            .is_empty()
    );
    // `find_player` / regex filter
    let re = regex::Regex::new("^u4").unwrap();
    assert!(
        bot.find_player(PlayerFilter::Matches(&re))
            .map(|e| e.id() == own.id())
            .unwrap_or(false)
    );

    // `nearest_entity` skips the bot itself (upstream semantics); whatever
    // it returns must be a different entity
    if let Some(nearest) = bot.nearest_entity(|_| true) {
        assert_ne!(nearest.id(), own.id());
    }

    // not riding anything; `vehicle` is None and `dismount` surfaces
    // upstream's error event instead of panicking
    assert!(bot.vehicle().is_none());
    bot.dismount();
    wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::Error(msg) if msg.contains("not mounted")),
        "dismount error event",
    )
    .await;

    // `bot.attack` needs a valid *other* entity — attacking our own
    // entity gets the connection kicked (`invalid_entity_attacked`) — so
    // bring up a second bot as the target
    let dummy = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u4dummy")
        .connect()
        .await
        .expect("dummy connect failed");
    let mut drx = dummy.events();
    wait_for(
        &mut drx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "dummy spawn",
    )
    .await;
    let mut dummy_entity = None;
    for _ in 0..100 {
        if let Some(e) = bot
            .find_players(PlayerFilter::Username("u4dummy"))
            .into_iter()
            .next()
        {
            dummy_entity = Some(e);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let dummy_entity = dummy_entity.expect("u4bot never saw the dummy's entity");

    bot.attack(&dummy_entity, true);
    bot.swing_arm(InteractionHand::MainHand);
    bot.swing_arm(InteractionHand::OffHand);
    bot.activate_entity(&dummy_entity)
        .await
        .expect("activate_entity failed");
    bot.use_on(&dummy_entity);
    bot.activate_entity_at(&dummy_entity, dummy_entity.position())
        .await
        .expect("activate_entity_at failed");
    assert!(own.on_ground().is_some());
    assert!(own.velocity().is_some());
    assert!(own.look_direction().is_some());

    // equipment/attributes start empty for our own entity (vanilla only
    // sends them for others), `firework_rocket_duration` is 0
    let _ = bot.equipment(&own);
    assert_eq!(bot.firework_rocket_duration(), 0);
    // explosion estimate on ourselves resolves (armor may be unknown, so
    // only the raw estimate is asserted)
    assert!(
        bot.get_explosion_damages(&own, own.position(), 1.0, true)
            .is_some()
    );

    bot.quit(None);
    dummy.quit(None);
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
}
