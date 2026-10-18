//! Rename or combine items in a nearby anvil. Port of mineflayer's
//! `examples/anvil.js`.
//!
//! ```sh
//! cargo run --example anvil -- <host> <port> [<name>]
//! ```
//! Chat: `list`, `xp`, `anvil rename <item> <name>`,
//! `anvil combine <item> <item> [<name>]`.

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("anvil", "anvilbot").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() {
            continue;
        }
        let words: Vec<&str> = message.split(' ').collect();
        let reply = match words.as_slice() {
            ["list"] => bot
                .inventory()
                .items()
                .iter()
                .map(|i| format!("{} x {}", i.name(), i.count()))
                .collect::<Vec<_>>()
                .join(", "),
            ["xp"] => bot.experience().level.to_string(),
            ["anvil", "rename", item, name @ ..] if !name.is_empty() => {
                rename(&bot, item, &name.join(" ")).await
            }
            ["anvil", "combine", one, two, name @ ..] => {
                let name = name.join(" ");
                combine(&bot, one, two, (!name.is_empty()).then_some(name.as_str())).await
            }
            _ => continue,
        };
        let _ = bot.chat(&reply);
    }
}

fn item(bot: &Bot, name: &str) -> Option<Item> {
    bot.inventory()
        .items()
        .into_iter()
        .find(|item| item.name().trim_start_matches("minecraft:") == name)
}

async fn open(bot: &Bot) -> Result<mineflayer::Anvil> {
    let block = bot
        .find_block(FindBlocksOpts::new(vec![
            BlockKind::Anvil,
            BlockKind::ChippedAnvil,
            BlockKind::DamagedAnvil,
        ]))
        .ok_or_else(|| Error::NotFound("no anvil nearby".into()))?;
    bot.open_anvil(&block).await
}

async fn rename(bot: &Bot, item_name: &str, name: &str) -> String {
    let Some(item) = item(bot, item_name) else {
        return format!("I have no {item_name}");
    };
    let result = match open(bot).await {
        Ok(anvil) => {
            let result = anvil.rename(&item, name).await;
            anvil.close();
            result
        }
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => format!("renamed {item_name} to {name}"),
        Err(err) => err.to_string(),
    }
}

async fn combine(bot: &Bot, one: &str, two: &str, name: Option<&str>) -> String {
    let (Some(one), Some(two)) = (item(bot, one), item(bot, two)) else {
        return "I don't have those items".into();
    };
    let result = match open(bot).await {
        Ok(anvil) => {
            let result = anvil.combine(&one, &two, name).await;
            anvil.close();
            result
        }
        Err(err) => Err(err),
    };
    match result {
        Ok(()) => "combined".into(),
        Err(err) => err.to_string(),
    }
}
