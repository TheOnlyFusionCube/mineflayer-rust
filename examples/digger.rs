//! Digger bot: digs the block below its feet and columns back up.
//! Port of mineflayer's `examples/digger.js` — chat commands:
//! `loaded`, `list`, `dig`, `build`, `equip dirt`.
//!
//! ```sh
//! cargo run --example digger -- <host> <port> [<name>]
//! ```

use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("Usage: cargo run --example digger -- <host> <port> [<name>]");
        std::process::exit(1);
    }
    let username = args.get(3).cloned().unwrap_or_else(|| "digger".to_string());

    println!("connecting as {username}");
    let bot = Bot::builder()
        .host(&args[1])
        .port(args[2].parse().expect("port must be a number"))
        .username(username)
        .connect()
        .await
        .expect("connect failed");

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
                let bot = bot.clone();
                // Commands are fire-and-forget tasks, like upstream's
                // un-awaited async handlers — the event loop keeps draining.
                tokio::spawn(async move {
                    match message.as_str() {
                        "loaded" => {
                            if bot.wait_for_chunks_to_load().await.is_ok() {
                                let _ = bot.chat("Ready!");
                            }
                        }
                        "list" => say_items(&bot, None),
                        "dig" => dig(&bot).await,
                        "build" => build(&bot).await,
                        "equip dirt" => equip_dirt(&bot).await,
                        _ => {}
                    }
                });
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

fn item_to_string(item: Option<&Item>) -> String {
    match item {
        Some(item) => format!("{} x {}", item.name(), item.count()),
        None => "(nothing)".to_string(),
    }
}

fn say_items(bot: &Bot, items: Option<Vec<Item>>) {
    let items = items.unwrap_or_else(|| bot.inventory().items());
    let output = items
        .iter()
        .map(|item| item_to_string(Some(item)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = bot.chat(if output.is_empty() { "empty" } else { &output });
}

async fn dig(bot: &Bot) {
    if let Some(target) = bot.target_dig_block() {
        let _ = bot.chat(&format!("already digging {}", target.short_name()));
        return;
    }
    // Upstream digs `bot.entity.position.offset(0, -1, 0)` — the block
    // directly under the bot's feet.
    let below = Vec3 {
        y: bot.position().y - 1.0,
        ..bot.position()
    }
    .to_block_pos_floor();
    let Some(target) = bot.block_at(below) else {
        let _ = bot.chat("cannot dig");
        return;
    };
    if !bot.can_dig_block(&target) {
        let _ = bot.chat("cannot dig");
        return;
    }
    let name = target.short_name();
    let _ = bot.chat(&format!("starting to dig {name}"));
    match bot.dig(&target).await {
        Ok(()) => {
            let _ = bot.chat(&format!("finished digging {name}"));
        }
        Err(err) => eprintln!("dig failed: {err}"),
    }
}

async fn build(bot: &Bot) {
    // Place a block under the feet at the apex of a jump — upstream's
    // dirt-column trick, driven by the `move` event.
    let below = Vec3 {
        y: bot.position().y - 1.0,
        ..bot.position()
    }
    .to_block_pos_floor();
    let Some(reference) = bot.block_at(below) else {
        let _ = bot.chat("no block under me to build on");
        return;
    };
    let jump_y = bot.position().y.floor() + 1.0;
    bot.set_control_state(Control::Jump, true);

    let mut events = bot.events();
    let mut try_count = 0;
    loop {
        match events.recv().await {
            Ok(BotEvent::Move { .. }) if bot.position().y > jump_y => {
                match bot.place_block(&reference, Vec3::new(0.0, 1.0, 0.0)).await {
                    Ok(_) => {
                        bot.set_control_state(Control::Jump, false);
                        let _ = bot.chat("Placing a block was successful");
                        return;
                    }
                    Err(err) => {
                        try_count += 1;
                        if try_count > 10 {
                            let _ = bot.chat(&err.to_string());
                            bot.set_control_state(Control::Jump, false);
                            return;
                        }
                    }
                }
            }
            Ok(BotEvent::End(_)) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                bot.set_control_state(Control::Jump, false);
                return;
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
    }
}

async fn equip_dirt(bot: &Bot) {
    // Upstream resolves `bot.registry[itemsByName].dirt.id` then equips it;
    // the kind-based equip is the same call without the registry lookup.
    match bot.equip_kind(ItemKind::Dirt, EquipDest::Hand).await {
        Ok(()) => {
            let _ = bot.chat("equipped dirt");
        }
        Err(err) => {
            let _ = bot.chat(&format!("unable to equip dirt: {err}"));
        }
    }
}
