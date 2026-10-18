//! Fish on command: `start` casts again after each catch is collected,
//! `stop` reels in, and `eat` eats a fish. Port of mineflayer's
//! `examples/fisherman.js`.
//!
//! ```sh
//! cargo run --example fisherman -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("fisherman", "fisherman").await;
    let mut events = bot.event_stream();
    let mut fishing: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(event) = events.next().await {
        match event {
            BotEvent::MessageStr { message, .. } => {
                if message.contains("start") {
                    stop(&bot, &mut fishing);
                    fishing = Some(tokio::spawn(fish(bot.clone())));
                } else if message.contains("stop") {
                    stop(&bot, &mut fishing);
                } else if message.contains("eat") {
                    stop(&bot, &mut fishing);
                    eat(&bot).await;
                }
            }
            // the catch reached the inventory: cast again
            BotEvent::PlayerCollect {
                collector: Some(collector),
                ..
            } if collector == bot.entity() && fishing.as_ref().is_some_and(|f| f.is_finished()) => {
                fishing = Some(tokio::spawn(fish(bot.clone())));
            }
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}

async fn fish(bot: Bot) {
    let _ = bot.chat("Fishing");
    if let Err(err) = bot.equip_kind(ItemKind::FishingRod, EquipDest::Hand).await {
        let _ = bot.chat(&err.to_string());
        return;
    }
    if let Err(err) = bot.fish().await {
        let _ = bot.chat(&err.to_string());
    }
}

fn stop(bot: &Bot, fishing: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(task) = fishing.take()
        && !task.is_finished()
    {
        task.abort();
        // reel the line in
        bot.activate_item(false);
    }
}

async fn eat(bot: &Bot) {
    if let Err(err) = bot.equip_kind(ItemKind::Cod, EquipDest::Hand).await {
        let _ = bot.chat(&err.to_string());
        return;
    }
    if let Err(err) = bot.consume().await {
        let _ = bot.chat(&err.to_string());
    }
}
