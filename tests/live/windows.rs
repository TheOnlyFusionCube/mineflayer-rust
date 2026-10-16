//! Live typed-window test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u9bot`.
//!
//! The survival fixture can't place real containers (no op, no console
//! stdin in live-server.sh, no furnace/anvil/table drops without
//! crafting), so the honest coverage here is:
//!
//! - `open_enchantment_table`'s block-name pre-check errors immediately
//!   on a non-table block (upstream asserts before opening).
//! - `open_furnace`/`open_anvil`/`open_villager` on the wrong target
//!   error (no window ever opens / wrong entity kind).
//! - `set_command_block` rejects a non-creative bot and a non-command
//!   block, `write_book`/`sign_book` reject bad slots and missing
//!   books — all upstream-style asserts.
//! - If the fixture world happens to have a furnace within reach it is
//!   opened for real: kind check, empty slot reads, `fuel()`/`progress()`.
//!
//! Packet-decode coverage (merchant offers, furnace ratios, enchant
//! clues, anvil choreography) lives in `src/window_types.rs`'s unit
//! tests; live coverage of real containers needs a fixture upgrade.
//!
//! Only compiled with `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1 (single `live` binary)
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::wait_for;
use azalea::{
    BlockPos,
    entity::{EntityKindComponent, Position, inventory::Inventory},
    registry::builtin::{BlockKind, EntityKind},
};
use mineflayer::{Bot, BotEvent, CommandBlockOpts, FindBlocksOpts};

/// The nearest loaded non-player, non-villager entity, if any.
fn non_villager_entity(bot: &Bot) -> Option<azalea::EntityRef> {
    bot.nearest_entity(|e| {
        e.get_component::<EntityKindComponent>()
            .is_some_and(|k| k.0 != EntityKind::Villager && k.0 != EntityKind::Player)
            && e.get_component::<Position>().is_some()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn typed_window_openers_and_guards() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u9bot")
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

    // a plain block under the feet — guaranteed loaded and never a
    // container
    let feet = bot.position().to_block_pos_floor();
    let below = bot
        .block_at(BlockPos {
            x: feet.x,
            y: feet.y - 1,
            z: feet.z,
        })
        .expect("block below feet unloaded");
    eprintln!("guard-test block: {}", below.name());

    // ---- pre-checks: wrong block/entity types error upstream-style ----
    // enchanting-table opener asserts the block name before opening
    let err = bot
        .open_enchantment_table(&below)
        .await
        .err()
        .expect("non-table block should not open");
    eprintln!("open_enchantment_table(dirt-ish): {err}");

    // villager opener asserts the entity kind before interacting
    if let Some(entity) = non_villager_entity(&bot) {
        let kind = entity
            .get_component::<EntityKindComponent>()
            .map(|k| k.0)
            .unwrap();
        let err = bot
            .open_villager(&entity)
            .await
            .err()
            .expect("non-villager entity should not open");
        eprintln!("open_villager({kind:?}): {err}");
    } else {
        eprintln!("no non-villager entity nearby; villager type-check skipped");
    }

    // setCommandBlock needs creative — the survival bot errors before
    // the block check like upstream's assert order
    let err = bot
        .set_command_block(feet, "say hi", CommandBlockOpts::default())
        .expect_err("survival bot cannot set command blocks");
    eprintln!("set_command_block(survival): {err}");

    // writeBook guards: slot range first, then "no book found"
    assert!(bot.write_book(45, vec!["x".into()]).await.is_err());
    assert!(bot.write_book(-1, vec!["x".into()]).await.is_err());
    let inv = bot.client().get_component::<Inventory>().unwrap();
    let empty_slot = (9..45)
        .find(|&i| inv.inventory_menu.slot(i).is_none_or(|s| s.is_empty()))
        .expect("inventory unexpectedly full");
    drop(inv);
    let err = bot
        .write_book(empty_slot as i32, vec!["x".into()])
        .await
        .expect_err("empty slot has no book");
    eprintln!("write_book(empty slot): {err}");
    let err = bot
        .sign_book(empty_slot as i32, vec!["x".into()], "u9bot", "t")
        .await
        .expect_err("empty slot has no book to sign");
    eprintln!("sign_book(empty slot): {err}");

    // ---- openers on the wrong block: the window never opens ----
    // (open_block waits 5 s for open_screen; right-clicking ground sends
    // nothing — the error is the honest no-window outcome)
    let err = bot
        .open_furnace(&below)
        .await
        .err()
        .expect("ground is not a furnace");
    eprintln!("open_furnace(ground): {err}");
    let err = bot
        .open_anvil(&below)
        .await
        .err()
        .expect("ground is not an anvil");
    eprintln!("open_anvil(ground): {err}");
    assert!(bot.current_window().is_none());

    // ---- opportunistic real furnace ----
    // A furnace within reach (e.g. a village structure near spawn) gets
    // the full open + read path; absent on the default fixture world.
    let furnace_pos = bot
        .find_blocks(
            FindBlocksOpts::new(BlockKind::Furnace)
                .max_distance(4.5)
                .count(1),
        )
        .into_iter()
        .next();
    if let Some(pos) = furnace_pos {
        let block = bot.block_at(pos).expect("furnace unloaded");
        let furnace = bot.open_furnace(&block).await.expect("open_furnace failed");
        eprintln!("opened real furnace at {pos} (id {})", furnace.id());
        assert_eq!(furnace.menu_kind(), Some(mineflayer::MenuKind::Furnace));
        // empty furnace: no items, no burn yet — reads are the zeroed
        // upstream defaults
        assert!(furnace.input_item().is_none());
        assert!(furnace.fuel_item().is_none());
        assert!(furnace.output_item().is_none());
        assert_eq!(furnace.fuel(), 0.0);
        assert_eq!(furnace.progress(), 0.0);
        // taking an empty slot errors like upstream's assert.ok(item)
        assert!(furnace.take_output().await.is_err());
        furnace.close();
        wait_for(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, BotEvent::WindowClose(_)),
            "windowClose after furnace close",
        )
        .await;
    } else {
        eprintln!("no furnace within reach on the fixture; real-open path skipped");
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    bot.quit(None);
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
}
