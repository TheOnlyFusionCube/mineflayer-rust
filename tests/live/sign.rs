//! Live sign test — connects to the local offline-mode fixture as
//! `u13bot`, gets an oak sign over the fixture's RCON, places it beside
//! itself, writes its front text, and reads the text back from the block
//! entity the server sends.

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::wait_for;
use mineflayer::{Bot, BotEvent, EquipDest, Vec3};
use mineflayer_engine::{BlockPos, registry::builtin::ItemKind};

/// One fixture console command (`scripts/live-server.sh rcon`).
fn rcon(command: &str) {
    let status = std::process::Command::new("bash")
        .args(["scripts/live-server.sh", "rcon", command])
        .status()
        .expect("run the fixture script");
    assert!(status.success(), "rcon {command:?} failed");
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn a_sign_reads_back_what_the_bot_wrote() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u13bot")
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
        .expect("spawn chunks load");

    rcon("give u13bot oak_sign 1");
    let mut has_sign = false;
    for _ in 0..100 {
        if bot.find_inventory_item(ItemKind::OakSign).is_some() {
            has_sign = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(has_sign, "the sign arrived in the inventory");
    bot.equip_kind(ItemKind::OakSign, EquipDest::Hand)
        .await
        .expect("equip the sign");

    // a cell near the bot, not its own column, with solid ground under it
    // and nothing in it a sign cannot replace (air, or a grass plant)
    let feet = BlockPos::from(bot.position());
    let free = |name: &str| {
        matches!(
            name,
            "minecraft:air" | "minecraft:short_grass" | "minecraft:fern"
        )
    };
    let around: Vec<(i32, i32)> = (-2..=2)
        .flat_map(|x| (-2..=2).map(move |z| (x, z)))
        .filter(|&(x, z)| (x, z) != (0, 0))
        .collect();
    let (ground, cell) = around
        .iter()
        .map(|&(x, z)| feet + BlockPos::new(x, 0, z))
        .find_map(|cell| {
            let ground = bot.block_at(cell + BlockPos::new(0, -1, 0))?;
            let solid = ground.bounding_box() == mineflayer::world::BoundingBox::Block
                && ground.diggable()
                && !free(ground.name());
            (solid && free(bot.block_at(cell)?.name())).then_some((ground, cell))
        })
        .unwrap_or_else(|| {
            let names: Vec<String> = around
                .iter()
                .filter_map(|&(x, z)| {
                    let cell = feet + BlockPos::new(x, 0, z);
                    Some(format!(
                        "{:?}: {} on {}",
                        (x, z),
                        bot.block_at(cell)?.name(),
                        bot.block_at(cell + BlockPos::new(0, -1, 0))?.name()
                    ))
                })
                .collect();
            panic!("no free cell near {feet:?}: {names:?}")
        });

    let mut events = bot.events();
    bot.place_block(&ground, Vec3::new(0., 1., 0.))
        .await
        .expect("place the sign");
    wait_for(
        &mut events,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::SignOpen { .. }),
        "the sign editor",
    )
    .await;
    let sign = bot.block_at(cell).expect("the placed sign");
    bot.update_sign(&sign, "hello\nlive", false)
        .expect("write the sign");
    // the server sends the empty sign when it is placed, then the text
    let written = |bot: &Bot| bot.sign_text(&sign, false).as_deref() == Some("hello\nlive\n\n");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !written(&bot) && tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout_at(deadline, events.recv()).await;
    }
    assert_eq!(
        bot.sign_text(&sign, false).as_deref(),
        Some("hello\nlive\n\n")
    );
    bot.quit(None);
}
