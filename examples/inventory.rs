//! Command the bot's inventory with chat: list, toss, equip, unequip, use,
//! and craft. Port of mineflayer's `examples/inventory.js`.
//!
//! ```sh
//! cargo run --example inventory -- <host> <port> [<name>]
//! ```
//! Chat: `loaded`, `list`, `toss [<count>] <item>`, `equip <dest> <item>`,
//! `unequip <dest>`, `use`, `craft <count> <item>`.

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("inventory", "inventory").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() {
            continue;
        }
        let words: Vec<&str> = message.split(' ').collect();
        let reply = match words.as_slice() {
            ["loaded"] => bot
                .wait_for_chunks_to_load()
                .await
                .map(|()| "Ready!".to_owned()),
            ["list"] => Ok(say_items(&bot)),
            ["toss", count, name] => toss(&bot, name, count.parse().ok()).await,
            ["toss", name] => toss(&bot, name, None).await,
            ["equip", dest, name] => equip(&bot, dest, name).await,
            ["unequip", dest] => match destination(dest) {
                Some(dest) => bot.unequip(dest).await.map(|()| "unequipped".into()),
                None => Ok(format!("no destination {dest}")),
            },
            ["use"] => {
                bot.activate_item(false);
                bot.deactivate_item();
                Ok("used".into())
            }
            ["craft", count, name] => craft(&bot, name, count.parse().unwrap_or(1)).await,
            _ => continue,
        };
        let _ = bot.chat(&reply.unwrap_or_else(|err| err.to_string()));
    }
}

fn say_items(bot: &Bot) -> String {
    let items: Vec<String> = bot
        .inventory()
        .items()
        .iter()
        .map(|item| format!("{} x {}", item.name(), item.count()))
        .collect();
    if items.is_empty() {
        "empty".into()
    } else {
        items.join(", ")
    }
}

fn item_by_name(bot: &Bot, name: &str) -> Option<Item> {
    bot.inventory()
        .items()
        .into_iter()
        .find(|item| item.name().trim_start_matches("minecraft:") == name)
}

async fn toss(bot: &Bot, name: &str, count: Option<i32>) -> Result<String> {
    let Some(item) = item_by_name(bot, name) else {
        return Ok(format!("I have no {name}"));
    };
    match count {
        Some(count) => {
            bot.toss(item.kind(), None, Some(count), None).await?;
            Ok(format!("tossed {count} x {name}"))
        }
        None => {
            bot.toss_stack(&item).await?;
            Ok(format!("tossed {name}"))
        }
    }
}

fn destination(name: &str) -> Option<EquipDest> {
    Some(match name {
        "hand" => EquipDest::Hand,
        "off-hand" => EquipDest::OffHand,
        "head" => EquipDest::Head,
        "torso" => EquipDest::Torso,
        "legs" => EquipDest::Legs,
        "feet" => EquipDest::Feet,
        _ => return None,
    })
}

async fn equip(bot: &Bot, dest: &str, name: &str) -> Result<String> {
    let (Some(dest), Some(item)) = (destination(dest), item_by_name(bot, name)) else {
        return Ok(format!("I have no {name} or no {dest}"));
    };
    bot.equip(&item, dest).await?;
    Ok(format!("equipped {name}"))
}

async fn craft(bot: &Bot, name: &str, count: i32) -> Result<String> {
    let Ok(kind) = name.parse::<ItemKind>() else {
        return Ok(format!("unknown item: {name}"));
    };
    let table = bot.find_block(FindBlocksOpts::new(BlockKind::CraftingTable).max_distance(4.));
    let Some(recipe) = bot
        .recipes_for(kind, None, Some(1), table.as_ref())
        .into_iter()
        .next()
    else {
        return Ok(format!("I cannot make {name}"));
    };
    bot.craft(&recipe, Some(count), table.as_ref()).await?;
    Ok(format!("did the recipe for {name} {count} times"))
}
