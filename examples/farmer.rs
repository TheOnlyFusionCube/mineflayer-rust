//! Harvest ripe wheat and sow seeds on free farmland nearby, once a
//! second. Port of mineflayer's `examples/farmer.js`.
//!
//! ```sh
//! cargo run --example farmer -- <host> <port> [<name>]
//! ```

mod common;

use std::time::Duration;

use mineflayer::{BlockMatcher, prelude::*};

#[tokio::main]
async fn main() {
    let bot = common::connect("farmer", "farmer").await;
    loop {
        if let Err(err) = farm(&bot).await {
            eprintln!("{err}");
        }
        if !bot.logged_in() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn farm(bot: &Bot) -> Result<()> {
    while let Some(wheat) = bot.find_block(
        FindBlocksOpts::new(BlockMatcher::Predicate(Box::new(|block| {
            block.kind() == BlockKind::Wheat && block.property("age") == Some("7")
        })))
        .point(bot.position())
        .max_distance(6.),
    ) {
        bot.dig(&wheat).await?;
    }
    loop {
        let world = bot.clone();
        let Some(farmland) = bot.find_block(
            FindBlocksOpts::new(BlockMatcher::Predicate(Box::new(move |block| {
                block.kind() == BlockKind::Farmland
                    && world
                        .block_at(block.pos.up(1))
                        .is_none_or(|above| above.state.is_air())
            })))
            .point(bot.position())
            .max_distance(6.),
        ) else {
            return Ok(());
        };
        bot.equip_kind(ItemKind::WheatSeeds, EquipDest::Hand)
            .await?;
        bot.place_block(&farmland, Vec3::new(0., 1., 0.)).await?;
    }
}
