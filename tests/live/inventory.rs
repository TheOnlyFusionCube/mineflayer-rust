//! Live inventory test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) as `u8bot` and exercises the U8
//! surface: the player-inventory `Window`, quickbar selection +
//! `heldItemChanged`, equipment-destination slots, click validation, and
//! `currentWindow`/`usingHeldItem` reads. Non-destructive: no blocks are
//! placed and no items are dropped. Only compiled with `--features live`;
//! run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live-inventory -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use mineflayer::{Bot, BotEvent, EquipDest, QUICK_BAR_START};
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

async fn connect() -> Bot {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u8bot")
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
    bot
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn inventory_reads_and_quickbar() {
    let bot = connect().await;
    let mut rx = bot.events();

    // the player inventory window is always available
    let inventory = bot.inventory();
    assert_eq!(inventory.id(), 0);
    assert_eq!(inventory.menu_kind(), None);
    assert!(inventory.is_open());
    // 46 slots: 0 craft result, 1..=4 grid, 5..=8 armor, 9..=35 main,
    // 36..=44 hotbar, 45 offhand
    let slots = inventory.slots().expect("player menu should be readable");
    assert_eq!(slots.len(), 46);
    assert_eq!(inventory.player_range(), 9..45);
    assert_eq!(inventory.container_range(), 0..9);
    // items()/inventory_items() read the live menu — empty at spawn
    assert_eq!(inventory.items().len(), inventory.inventory_items().len());

    // no non-inventory window is open at spawn
    assert!(bot.current_window().is_none());

    // quickbar starts at slot 0 on a fresh login; held item is empty
    assert_eq!(bot.quick_bar_slot(), 0);
    assert_eq!(QUICK_BAR_START, 36);
    assert!(bot.held_item().is_none());
    assert!(!bot.using_held_item());

    // selecting a slot emits heldItemChanged and updates reads
    bot.set_quick_bar_slot(4).await.unwrap();
    assert_eq!(bot.quick_bar_slot(), 4);
    wait_for(
        &mut rx,
        Duration::from_secs(5),
        |e| matches!(e, BotEvent::HeldItemChanged(_)),
        "heldItemChanged after set_quick_bar_slot",
    )
    .await;

    // out-of-range selection errors like upstream's assert
    assert!(bot.set_quick_bar_slot(9).await.is_err());

    // equipment destinations map to player-window slots
    assert_eq!(
        bot.equipment_dest_slot(EquipDest::Hand),
        QUICK_BAR_START as u16 + 4
    );
    assert_eq!(bot.equipment_dest_slot(EquipDest::Head), 5);
    assert_eq!(bot.equipment_dest_slot(EquipDest::Torso), 6);
    assert_eq!(bot.equipment_dest_slot(EquipDest::Legs), 7);
    assert_eq!(bot.equipment_dest_slot(EquipDest::Feet), 8);
    assert_eq!(bot.equipment_dest_slot(EquipDest::OffHand), 45);

    // activate_item on an empty hand is a silent no-op (upstream)
    bot.activate_item(false).await;
    assert!(!bot.using_held_item());
    bot.deactivate_item().await;

    // click_window validation: bad mode errors, -999 outside-click is ok
    assert!(bot.click_window(0, 0, 7).await.is_err());
    assert!(bot.click_window(-999, 0, 0).await.is_ok());

    // toss with nothing to move errors like upstream's "Can't find"
    assert!(
        bot.toss(mineflayer::ItemKind::Diamond, None, Some(1), None)
            .await
            .is_err()
    );

    bot.quit(None);
}
