//! List nearby villagers and their trades, and trade, on chat commands.
//! Port of mineflayer's `examples/trader.js`.
//!
//! ```sh
//! cargo run --example trader -- <host> <port> [<name>]
//! ```
//! Chat: `show villagers`, `show inventory`, `show trades <id>`,
//! `trade <id> <trade> [<times>]`.

mod common;

use futures::StreamExt;
use mineflayer::{
    engine::{
        core::entity_id::MinecraftEntityId, entity::EntityKindComponent,
        registry::builtin::EntityKind,
    },
    prelude::*,
};

#[tokio::main]
async fn main() {
    let bot = common::connect("trader", "trader").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() {
            continue;
        }
        let words: Vec<&str> = message.split(' ').collect();
        let reply = match words.as_slice() {
            ["show", "villagers"] => show_villagers(&bot),
            ["show", "inventory"] => bot
                .inventory()
                .items()
                .iter()
                .map(|i| format!("{} x {}", i.name(), i.count()))
                .collect::<Vec<_>>()
                .join(", "),
            ["show", "trades", id] => show_trades(&bot, id).await,
            ["trade", id, index] => trade(&bot, id, index, None).await,
            ["trade", id, index, times] => trade(&bot, id, index, times.parse().ok()).await,
            _ => continue,
        };
        let _ = bot.chat(&reply);
    }
}

fn villagers(bot: &Bot) -> Vec<(MinecraftEntityId, EntityRef)> {
    bot.entities()
        .into_iter()
        .filter(|(_, e)| {
            e.get_component::<EntityKindComponent>()
                .is_some_and(|k| k.0 == EntityKind::Villager)
        })
        .collect()
}

fn show_villagers(bot: &Bot) -> String {
    let all = villagers(bot);
    let close: Vec<String> = all
        .iter()
        .filter(|(_, e)| e.position().distance_to(bot.position()) < 3.)
        .map(|(id, _)| id.0.to_string())
        .collect();
    format!(
        "found {} villagers; villager(s) you can trade with: {}",
        all.len(),
        close.join(", ")
    )
}

/// The villager with protocol id `id`, within reach.
fn villager(bot: &Bot, id: &str) -> std::result::Result<EntityRef, String> {
    let id: i32 = id.parse().map_err(|_| format!("{id} is not an id"))?;
    let (_, entity) = villagers(bot)
        .into_iter()
        .find(|(eid, _)| eid.0 == id)
        .ok_or(format!("cant find villager with id {id}"))?;
    if entity.position().distance_to(bot.position()) > 3. {
        return Err("villager out of reach".into());
    }
    Ok(entity)
}

async fn show_trades(bot: &Bot, id: &str) -> String {
    let entity = match villager(bot, id) {
        Ok(entity) => entity,
        Err(reply) => return reply,
    };
    match bot.open_villager(&entity).await {
        Ok(villager) => {
            villager.close();
            villager
                .trades()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    format!(
                        "{}: {} x {} for {} x {}",
                        i + 1,
                        t.input_item_1.count(),
                        t.input_item_1.name(),
                        t.output_item.count(),
                        t.output_item.name()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        }
        Err(err) => err.to_string(),
    }
}

async fn trade(bot: &Bot, id: &str, index: &str, times: Option<i32>) -> String {
    let entity = match villager(bot, id) {
        Ok(entity) => entity,
        Err(reply) => return reply,
    };
    let Some(index) = index.parse::<usize>().ok().and_then(|i| i.checked_sub(1)) else {
        return "trade not found".into();
    };
    let villager = match bot.open_villager(&entity).await {
        Ok(villager) => villager,
        Err(err) => return err.to_string(),
    };
    let reply = match villager.trade(index, times).await {
        Ok(()) => "traded".into(),
        Err(err) => err.to_string(),
    };
    villager.close();
    reply
}
