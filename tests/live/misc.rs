//! Live misc-domains test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) and exercises the U11 surface
//! that a vanilla server produces deterministically: health packets,
//! settings get/set, resource-pack denial paths, bed/wake errors,
//! scoreboard/team/boss-bar emptiness. Only compiled with
//! `--features live`:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1 (single `live` binary)
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::wait_for;
use azalea::{BlockPos, entity::HumanoidArm, inventory::ItemStack, registry::builtin::ItemKind};
use mineflayer::{Bot, BotEvent};
use tokio::sync::broadcast::error::RecvError;

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn misc_domains() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u11bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();

    // vanilla sends set_health + set_experience in the login burst, before
    // Spawn — collect them while waiting so they aren't skipped
    let mut saw_health = false;
    let mut saw_experience = false;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match rx.recv().await {
                Ok(BotEvent::Spawn) => break,
                Ok(BotEvent::Health) => saw_health = true,
                Ok(BotEvent::Experience) => saw_experience = true,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("event stream closed waiting for spawn"),
            }
        }
    })
    .await
    .expect("timed out waiting for spawn");
    assert!(saw_health, "no Health event during login");
    assert!(saw_experience, "no Experience event during login");
    assert!(bot.health() > 0.0);

    // scoreboard/team/boss-bar state starts empty on a fresh join
    assert!(bot.scoreboards().is_empty());
    assert!(bot.teams().is_empty());
    assert!(bot.team_map().is_empty());
    assert!(bot.boss_bars().is_empty());
    // ScoreboardSlot::Sidebar lookup is empty too
    assert!(
        bot.scoreboard(mineflayer::ScoreboardSlot::Sidebar)
            .is_none()
    );

    // bed: not sleeping, can't wake, can't sleep in a non-bed block
    assert!(!bot.is_sleeping());
    assert!(bot.wake().is_err(), "wake while awake should error");
    let err = bot
        .sleep(BlockPos::new(0, 64, 0))
        .await
        .expect_err("sleeping in a non-bed must fail");
    assert!(!err.to_string().is_empty());

    // resource packs: no push is pending, so both answers error out
    assert!(bot.accept_resource_pack().is_err());
    assert!(bot.deny_resource_pack().is_err());

    // settings: defaults from azalea's ClientInformation…
    let settings = bot.settings();
    assert_eq!(settings.view_distance, 8);
    assert_eq!(settings.locale, "en_us");
    assert_eq!(settings.main_hand, HumanoidArm::Right);
    // …and set_settings merges — only the fields we set change
    bot.set_settings(mineflayer::Settings {
        view_distance: 6.into(),
        main_hand: HumanoidArm::Left,
        ..bot.settings()
    })
    .expect("set_settings failed");
    let settings = bot.settings();
    assert_eq!(settings.view_distance, 6);
    assert_eq!(settings.main_hand, HumanoidArm::Left);
    assert_eq!(settings.locale, "en_us", "merge must not reset fields");
    // and the invalid view distance is rejected like upstream
    assert!(
        bot.set_settings(mineflayer::Settings {
            view_distance: 0.into(),
            ..bot.settings()
        })
        .is_err()
    );

    // creative: a survival-mode server silently ignores creative slot
    // packets; the echo wait rejects after 5s like upstream. (The item
    // must be non-empty — Empty into an empty slot is a no-op.)
    assert!(matches!(
        bot.creative()
            .set_inventory_slot(9, ItemStack::new(ItemKind::Apple, 1))
            .await,
        Err(mineflayer::Error::Timeout)
    ));

    bot.quit(None);
    let end = wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
    assert!(matches!(end, BotEvent::End(r) if r == "disconnect.quitting"));
}
