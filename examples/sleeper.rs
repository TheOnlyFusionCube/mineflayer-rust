//! Find a bed and sleep in it, or wake up, on a chat command. Port of
//! mineflayer's `examples/sleeper.js`.
//!
//! ```sh
//! cargo run --example sleeper -- <host> <port> [<name>]
//! ```
//! Chat: `sleep`, `wakeup`.

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("sleeper", "sleeper").await;
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::Sleep => {
                let _ = bot.chat("Good night!");
            }
            BotEvent::Wake => {
                let _ = bot.chat("Good morning!");
            }
            BotEvent::Chat(packet) => {
                let (Some(username), message) = packet.split_sender_and_content() else {
                    continue;
                };
                if username == bot.username() {
                    continue;
                }
                match message.as_str() {
                    "sleep" => go_to_sleep(&bot).await,
                    "wakeup" => {
                        if let Err(err) = bot.wake() {
                            let _ = bot.chat(&format!("I can't wake up: {err}"));
                        }
                    }
                    _ => {}
                }
            }
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}

async fn go_to_sleep(bot: &Bot) {
    let beds = bot.clone();
    let bed = bot.find_block(FindBlocksOpts::new(mineflayer::BlockMatcher::Predicate(
        Box::new(move |block| beds.is_a_bed(block.state)),
    )));
    let Some(bed) = bed else {
        let _ = bot.chat("No nearby bed");
        return;
    };
    match bot.sleep(bed.pos).await {
        Ok(()) => {
            let _ = bot.chat("I'm sleeping");
        }
        Err(err) => {
            let _ = bot.chat(&format!("I can't sleep: {err}"));
        }
    }
}
