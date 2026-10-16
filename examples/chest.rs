//! Chest bot: opens a nearby chest, prints its contents, and moves items
//! in and out on chat command. Port of the chest part of mineflayer's
//! `examples/chest.js` — commands: `list`, `chest`, then while a chest is
//! open `close`, `withdraw <count> <name>`, `deposit <count> <name>`.
//! (Upstream's furnace/enchanting-table/minecart/invsee commands share the
//! same open→items→commands shape and are omitted.)
//!
//! ```sh
//! cargo run --example chest -- <host> <port> [<name>]
//! ```

use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("Usage: cargo run --example chest -- <host> <port> [<name>]");
        std::process::exit(1);
    }
    let username = args.get(3).cloned().unwrap_or_else(|| "chest".to_string());

    println!("connecting as {username}");
    let bot = Bot::builder()
        .host(&args[1])
        .port(args[2].parse().expect("port must be a number"))
        .username(username)
        .connect()
        .await
        .expect("connect failed");

    // The currently watched chest, if any — upstream's per-chest listeners
    // become state in the single event loop.
    let mut chest: Option<Window> = None;

    let mut events = bot.events();
    loop {
        match events.recv().await {
            Ok(BotEvent::Chat(packet)) => {
                let (Some(sender), message) = packet.split_sender_and_content() else {
                    continue;
                };
                if sender == bot.username() {
                    continue;
                }
                let command: Vec<&str> = message.split(' ').collect();
                match command.as_slice() {
                    ["list"] => say_items(&bot, bot.inventory().items()),
                    ["chest"] => {
                        if let Some(open) = &chest {
                            open.close();
                        }
                        chest = watch_chest(&bot).await;
                    }
                    ["close"] => {
                        if let Some(open) = &chest {
                            // `close()` emits `WindowClose` once; leave
                            // `chest` set so the event arm prints "chest
                            // closed" and clears it — same ordering as
                            // upstream's close listener.
                            open.close();
                        }
                    }
                    ["withdraw", amount, name] => {
                        if let Some(open) = &chest {
                            withdraw_item(&bot, open, name, amount).await;
                        }
                    }
                    ["deposit", amount, name] => {
                        if let Some(open) = &chest {
                            deposit_item(&bot, open, name, amount).await;
                        }
                    }
                    _ => {}
                }
            }
            Ok(BotEvent::Experience) => {
                let _ = bot.chat(&format!("I am level {}", bot.experience().level));
            }
            // Upstream's `chest.on('updateSlot')`/`('close')` — window events
            // are bot-level here, filtered by the window's protocol id.
            Ok(BotEvent::WindowUpdateSlot {
                window_id,
                slot,
                old,
                new,
            }) if chest.as_ref().is_some_and(|open| open.id() == window_id) => {
                let _ = bot.chat(&format!(
                    "chest update: {} -> {} (slot: {slot})",
                    item_to_string(old.as_ref()),
                    item_to_string(new.as_ref())
                ));
            }
            Ok(BotEvent::WindowClose(window))
                if chest.as_ref().is_some_and(|open| open.id() == window.id()) =>
            {
                let _ = bot.chat("chest closed");
                chest = None;
            }
            Ok(BotEvent::End(reason)) => {
                println!("disconnected: {reason}");
                break;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// `chest` command: find a chest-like block within 6 blocks, open it, and
/// chat its contents. Returns the window so the caller can route
/// follow-up commands to it.
async fn watch_chest(bot: &Bot) -> Option<Window> {
    // Upstream matches resolved block ids (`blocksByName[name].id`) — the
    // kind set is the equivalent here. (`Block::name()` is namespaced, so
    // a name-string match never fires.)
    let block = bot.find_block(
        FindBlocksOpts::new([
            BlockKind::Chest,
            BlockKind::EnderChest,
            BlockKind::TrappedChest,
        ])
        .max_distance(6.0),
    );
    let Some(block) = block else {
        let _ = bot.chat("no chest found");
        return None;
    };
    match bot.open_container(&block).await {
        Ok(open) => {
            say_items(bot, open.container_items());
            Some(open)
        }
        Err(err) => {
            let _ = bot.chat(&format!(
                "cannot open {name}: {err}",
                name = block.short_name()
            ));
            None
        }
    }
}

async fn withdraw_item(bot: &Bot, chest: &Window, name: &str, amount: &str) {
    let Ok(amount) = amount.parse::<i32>() else {
        return;
    };
    let Some(item) = chest
        .container_items()
        .iter()
        .find(|i| i.name() == name)
        .cloned()
    else {
        let _ = bot.chat(&format!("unknown item {name}"));
        return;
    };
    match chest.withdraw(item.kind(), None, Some(amount), None).await {
        Ok(()) => {
            let _ = bot.chat(&format!("withdrew {amount} {}", item.name()));
        }
        Err(_) => {
            let _ = bot.chat(&format!("unable to withdraw {amount} {}", item.name()));
        }
    }
}

async fn deposit_item(bot: &Bot, chest: &Window, name: &str, amount: &str) {
    let Ok(amount) = amount.parse::<i32>() else {
        return;
    };
    let Some(item) = chest.items().iter().find(|i| i.name() == name).cloned() else {
        let _ = bot.chat(&format!("unknown item {name}"));
        return;
    };
    match chest.deposit(item.kind(), None, Some(amount), None).await {
        Ok(()) => {
            let _ = bot.chat(&format!("deposited {amount} {}", item.name()));
        }
        Err(_) => {
            let _ = bot.chat(&format!("unable to deposit {amount} {}", item.name()));
        }
    }
}

fn say_items(bot: &Bot, items: Vec<Item>) {
    let output = items
        .iter()
        .map(|item| item_to_string(Some(item)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = bot.chat(if output.is_empty() { "empty" } else { &output });
}

fn item_to_string(item: Option<&Item>) -> String {
    match item {
        Some(item) => format!("{} x {}", item.name(), item.count()),
        None => "(nothing)".to_string(),
    }
}
