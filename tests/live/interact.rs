//! Live interact test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u6bot`, waits for spawn and
//! chunks, then exercises the block-interaction surface end to end:
//!
//! 1. `dig` the block directly below the feet — `diggingCompleted` fires,
//!    the cell turns to air, and the drop lands where the bot stands so it
//!    is picked up automatically.
//! 2. `stop_digging` mid-dig on another block — the dig rejects and
//!    `diggingAborted` fires, leaving the block intact.
//! 3. Hop out of the hole and `place_block` the drop back into the hole —
//!    `blockPlaced` fires and `block_at` confirms; the world is restored.
//! 4. `activate_block` on the placed block (right-click wire path).
//!
//! `place_entity` isn't exercised: it needs an armor stand / spawn egg /
//! boat in hand, which a vanilla no-op bot can't obtain.
//!
//! Only compiled with `--features live`; run with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live_interact -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use azalea::{
    BlockPos, Vec3,
    entity::{EntityKindComponent, Position, inventory::Inventory},
    registry::builtin::{BlockKind, EntityKind},
};
use mineflayer::{
    Bot, BotEvent, Control, DigAbortReason, DigFace, FindBlocksOpts, ForceLook, entity::EntityExt,
};
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

/// Point the held item at the first non-empty hotbar slot; false while the
/// inventory is still empty (the drop hasn't been picked up yet).
fn select_any_item(bot: &Bot) -> bool {
    let slot = bot.client().get_component::<Inventory>().and_then(|inv| {
        let menu = inv.menu();
        let range = menu.hotbar_slots_range();
        range
            .clone()
            .find(|&i| menu.slot(i).is_some_and(|s| s.is_present()))
            .map(|i| (i - range.start()) as u8)
    });
    match slot {
        Some(i) => {
            bot.client().set_selected_hotbar_slot(i);
            true
        }
        None => false,
    }
}

/// Walk toward the nearest `item` entity (the dug block's drop can scatter
/// a block or two) until something lands in the hotbar.
async fn collect_drop(bot: &Bot, timeout: Duration) -> bool {
    let until = std::time::Instant::now() + timeout;
    let mut walking = false;
    while std::time::Instant::now() < until {
        if select_any_item(bot) {
            break;
        }
        if let Some(drop) = bot.nearest_entity(|e| {
            e.get_component::<EntityKindComponent>()
                .is_some_and(|k| k.0 == EntityKind::Item)
        }) && let Some(pos) = drop.get_component::<Position>().map(|p| **p)
        {
            if !walking {
                bot.set_control_state(Control::Forward, true);
                bot.set_control_state(Control::Jump, true);
                walking = true;
            }
            bot.client().look_at(pos);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if walking {
        bot.set_control_state(Control::Forward, false);
        bot.set_control_state(Control::Jump, false);
    }
    select_any_item(bot)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn dig_place_activate_stop() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u6bot")
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

    // ---- dig a block in reach ---------------------------------------------
    // the persistent fixture has drifted around the spawn (holes, edges —
    // the bot can land on a one-wide column), so land first and then take
    // the first guaranteed-dropping block in reach: the cell under the
    // feet when it is real ground, else the nearest of the common droppers
    for _ in 0..100 {
        if bot.entity().on_ground() == Some(true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let below_pos = {
        let feet = bot.position().to_block_pos_floor();
        std::iter::once(BlockPos {
            x: feet.x,
            y: feet.y - 1,
            z: feet.z,
        })
        .chain(
            [
                BlockKind::Dirt,
                BlockKind::GrassBlock,
                BlockKind::Stone,
                BlockKind::Cobblestone,
                BlockKind::Sand,
                BlockKind::Gravel,
            ]
            .into_iter()
            .flat_map(|kind| bot.find_blocks(FindBlocksOpts::new(kind).max_distance(4.5).count(2))),
        )
        .find(|pos| bot.block_at(*pos).is_some_and(|b| bot.can_dig_block(&b)))
        .expect("no diggable block in reach of the spawn")
    };
    let below = bot.block_at(below_pos).expect("dig target unloaded");
    assert!(bot.can_dig_block(&below), "dig target out of reach");

    bot.dig(&below).await.expect("dig failed");
    match wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::DiggingCompleted(_)),
        "diggingCompleted",
    )
    .await
    {
        BotEvent::DiggingCompleted(new) => {
            assert!(new.state.is_air(), "dug cell isn't air: {}", new.name());
            assert_eq!(new.pos, below_pos);
        }
        _ => unreachable!(),
    }
    let after = bot.block_at(below_pos).expect("dug cell unloaded").state;
    assert!(after.is_air(), "block_at still sees {}", below.name());
    assert!(bot.last_dig_time().is_some(), "lastDigTime not recorded");
    assert!(bot.target_dig_block().is_none(), "still digging after dig");

    // the bot fell into the hole; sweep up the drop (it can scatter out of
    // the hole cell)
    let collected = collect_drop(&bot, Duration::from_secs(6)).await;
    assert!(collected, "the dug block's drop was never picked up");

    // ---- stop_digging mid-dig ---------------------------------------------
    // pick the slowest-digging block in reach so the abort always lands
    // mid-dig; aborting leaves the block intact so nothing needs restoring
    let feet2 = bot.position().to_block_pos_floor();
    let below2_pos = BlockPos {
        x: feet2.x,
        y: feet2.y - 1,
        z: feet2.z,
    };
    let mut candidates = vec![below2_pos];
    for kind in [
        BlockKind::Obsidian,
        BlockKind::Deepslate,
        BlockKind::Stone,
        BlockKind::Cobblestone,
        BlockKind::Sandstone,
        BlockKind::StoneBricks,
        BlockKind::EndStone,
    ] {
        candidates.extend(bot.find_blocks(FindBlocksOpts::new(kind).max_distance(4.).count(2)));
    }
    let (abort_pos, abort_dig_time) = candidates
        .iter()
        .filter_map(|&p| {
            let b = bot.block_at(p)?;
            b.diggable()
                .then(|| bot.dig_time(&b))
                .flatten()
                .map(|t| (p, t))
        })
        .max_by_key(|(_, t)| *t)
        .expect("no diggable block in reach for the abort test");
    assert!(
        abort_dig_time >= Duration::from_millis(600),
        "nothing slow enough to abort-dig (best {abort_dig_time:?} at {abort_pos})"
    );
    let abort_block = bot.block_at(abort_pos).expect("unloaded");
    eprintln!("abort-dig target: {abort_pos} dig_time {abort_dig_time:?}");

    let bot2 = bot.clone();
    let dig = tokio::spawn(async move {
        bot2.dig_opts(
            &abort_block,
            ForceLook::Force,
            DigFace::Vec(Vec3::new(0., 1., 0.)),
        )
        .await
    });
    // wait until the dig is actually registered, then abort it
    for _ in 0..50 {
        if bot.target_dig_block().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        bot.target_dig_block().is_some(),
        "dig never started (target_dig_block empty)"
    );
    assert!(bot.target_dig_face().is_some(), "targetDigFace missing");
    bot.stop_digging();
    let res = dig.await.expect("dig task panicked");
    assert!(res.is_err(), "stopped dig should reject, got {res:?}");
    match wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::DiggingAborted { .. }),
        "diggingAborted",
    )
    .await
    {
        BotEvent::DiggingAborted { block, reason } => {
            assert_eq!(block.pos, abort_pos);
            assert_eq!(reason, DigAbortReason::Stopped);
        }
        _ => unreachable!(),
    }
    // the aborted block is still there (abort, not break)
    assert!(
        !bot.block_at(abort_pos).expect("unloaded").state.is_air(),
        "aborted dig still broke the block"
    );

    // ---- hop out of the first hole ----------------------------------------
    // face the original ground level and auto-jump out
    bot.client().look_at(Vec3::new(
        below_pos.x as f64,
        below_pos.y as f64 + 2.,
        below_pos.z as f64 + 2.,
    ));
    bot.set_control_state(Control::Forward, true);
    bot.set_control_state(Control::Jump, true);
    let mut out = false;
    for _ in 0..60 {
        if bot.position().y > below_pos.y as f64 + 0.9 {
            out = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bot.set_control_state(Control::Forward, false);
    bot.set_control_state(Control::Jump, false);
    assert!(out, "bot couldn't climb out of the hole");

    // ---- place the drop back into the hole --------------------------------
    // reference = a solid side-wall of the hole, face pointing into it so
    // `dest` is the empty hole cell itself.
    let sides = [
        (
            BlockPos {
                x: below_pos.x + 1,
                ..below_pos
            },
            Vec3::new(-1., 0., 0.),
        ),
        (
            BlockPos {
                x: below_pos.x - 1,
                ..below_pos
            },
            Vec3::new(1., 0., 0.),
        ),
        (
            BlockPos {
                z: below_pos.z + 1,
                ..below_pos
            },
            Vec3::new(0., 0., -1.),
        ),
        (
            BlockPos {
                z: below_pos.z - 1,
                ..below_pos
            },
            Vec3::new(0., 0., 1.),
        ),
    ];
    let mut placed = None;
    let mut side_errs = Vec::new();
    for (ref_pos, face) in sides {
        let Some(reference) = bot.block_at(ref_pos) else {
            side_errs.push(format!("{ref_pos:?}: unloaded"));
            continue;
        };
        if reference.state.is_air() {
            side_errs.push(format!("{ref_pos:?}: air"));
            continue;
        }
        match bot.place_block(&reference, face).await {
            Ok(new) => {
                placed = Some(new);
                break;
            }
            Err(e) => side_errs.push(format!("{ref_pos:?}: {e:?}")),
        }
    }
    let placed = placed.unwrap_or_else(|| {
        panic!(
            "place_block failed on every side of the hole {below_pos:?} (dug {}; held {:?}): {side_errs:?}",
            below.name(),
            bot.held_item().map(|i| i.name())
        )
    });
    assert_eq!(placed.pos, below_pos, "placed block isn't the hole");
    match wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::BlockPlaced { .. }),
        "blockPlaced",
    )
    .await
    {
        BotEvent::BlockPlaced { new, .. } => assert_eq!(new.pos, below_pos),
        _ => unreachable!(),
    }
    let filled = bot.block_at(below_pos).expect("hole unloaded");
    assert_eq!(
        filled.kind(),
        placed.kind(),
        "block_at doesn't see the placed block"
    );

    // ---- activate_block ----------------------------------------------------
    // right-click the block we just placed (no item / nothing happens
    // server-side — the use_item_on wire path is what we're exercising)
    let refilled = bot.block_at(below_pos).expect("hole unloaded");
    bot.activate_block(&refilled, None, None)
        .await
        .expect("activate_block failed");

    // let queued packets drain before disconnecting — the bridge can hit a
    // wave-A teardown race (entity_by_minecraft_id panics once the local
    // entity is despawned) if packets are still mid-translation
    tokio::time::sleep(Duration::from_millis(1500)).await;
    bot.quit(None);
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
}
