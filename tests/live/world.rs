//! Live world/blocks test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u3bot`, waits for spawn and
//! chunks, then exercises the world surface read-only: `block_at`,
//! `find_blocks`, `can_see_block`, `block_at_cursor`,
//! `block_at_entity_cursor`, `time`, `game`, `spawn_point`.
//!
//! Only compiled with `--features live`; run with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live_world -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use azalea::registry::builtin::BlockKind;
use mineflayer::{Bot, BotEvent, FindBlocksOpts};
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Receive events until one satisfies `pred` (`Lagged` just re-reads).
/// Panics on timeout or a closed stream.
async fn wait_for(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> BotEvent {
    tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("event stream closed while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn world_blocks() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u3bot")
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
    bot.wait_for_chunks_to_load()
        .await
        .expect("chunks never loaded");

    // `game` — filled from the login packet + difficulty packet + brand.
    let game = bot.game();
    assert_eq!(game.hardcore, Some(false), "fixture isn't hardcore");
    assert_eq!(game.max_players, Some(20), "vanilla default max players");
    assert!(game.difficulty.is_some(), "difficulty packet decoded");
    assert!(game.game_mode.is_some(), "gamemode component present");
    assert!(game.level_type.is_some(), "is_flat from login decoded");
    assert_eq!(game.min_y, Some(-64));
    assert_eq!(game.height, Some(384));
    assert!(game.dimension.is_some());
    // `server_brand` stays `None` here: vanilla sends `minecraft:brand`
    // during the *configuration* phase, which azalea's `Event::Packet`
    // (game packets only) never exposes. The game-state decode arm stays
    // for servers that re-send it in-game.

    // `blockAt` — the block below the feet must resolve to a real block.
    let feet = bot.position().to_block_pos_floor();
    let below_pos = azalea::BlockPos {
        x: feet.x,
        y: feet.y - 1,
        z: feet.z,
    };
    let below = bot.block_at(below_pos).expect("block below feet");
    assert_ne!(below.name(), "air");
    assert_eq!(below.kind(), below.state.as_block_kind());
    assert_eq!(below.bounding_box(), "block");
    assert_eq!(below.state_id(), below.state.id() as u32);

    // `findBlocks` — grass or dirt within 16, closest-first.
    let found = bot.find_blocks(
        FindBlocksOpts::new([BlockKind::GrassBlock, BlockKind::Dirt])
            .max_distance(16.)
            .count(4),
    );
    assert!(!found.is_empty(), "no grass/dirt found near spawn");
    let distances: Vec<f64> = found.iter().map(|p| p.distance_to(feet)).collect();
    let mut sorted = distances.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(distances, sorted, "find_blocks isn't closest-first");

    // `findBlock` returns the closest hit as a `Block`.
    let nearest = bot.find_block(
        FindBlocksOpts::new([BlockKind::GrassBlock, BlockKind::Dirt]).max_distance(16.),
    );
    assert_eq!(nearest.map(|b| b.pos), found.first().copied());

    // predicate matching — same result, spelled as a `Fn(&Block)`.
    let pred_found = bot.find_blocks(
        FindBlocksOpts::new(|b: &mineflayer::Block| {
            b.kind() == BlockKind::GrassBlock || b.kind() == BlockKind::Dirt
        })
        .max_distance(16.)
        .count(4),
    );
    assert!(
        !pred_found.is_empty(),
        "predicate find_blocks found nothing"
    );

    // `canSeeBlock` — the block under the feet is in line of sight.
    assert!(
        bot.can_see_block(&below),
        "can't see the block directly below"
    );

    // `blockAtEntityCursor`/`blockAtCursor` — look straight down; the ray
    // must land on the block under the feet.
    let me = bot.client().entity_ref_for(bot.client().entity);
    bot.client().look_at(bot.position().down(4.));
    bot.wait_for_ticks(3).await.expect("wait_for_ticks");
    let under_cursor = bot
        .block_at_cursor(16., None)
        .expect("block_at_cursor after looking down");
    assert_eq!(under_cursor.pos, below_pos);
    let from_entity = bot
        .block_at_entity_cursor(&me, 16., None)
        .expect("block_at_entity_cursor after looking down");
    assert_eq!(from_entity.pos, below_pos);
    // Staring at the ground: no entity should be in the crosshair.
    assert!(bot.entity_at_cursor(3.5).is_none());

    // `spawnPoint` — decoded from c_set_default_spawn_position (azalea
    // drops it). This fixture doesn't send the packet on join (verified
    // via a packet dump), so only the accessor is exercised.
    let _spawn = bot.spawn_point();

    // `blockUpdate` events — the fixture emits natural block updates
    // (section batch updates during chunk load tickle the same decoder
    // path), so a BlockUpdate must arrive shortly after spawn.
    wait_for(
        &mut rx,
        Duration::from_secs(30),
        |e| matches!(e, BotEvent::BlockUpdate { .. }),
        "block update",
    )
    .await;

    // `time` — vanilla re-sends set_time roughly once a second; the event
    // proves the packet bridge decoded it.
    wait_for(
        &mut rx,
        Duration::from_secs(30),
        |e| matches!(e, BotEvent::Time),
        "time update",
    )
    .await;
    let time = bot.time().expect("time() after Time event");
    assert!(time.time_of_day < 24000);
    assert_eq!(time.day, time.time / 24000);
    assert_eq!(time.moon_phase, time.day % 8);
    assert!(!bot.is_raining());
    assert_eq!(bot.rain_state(), 0.);
    assert_eq!(bot.thunder_state(), 0.);

    bot.quit(None);
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
}
