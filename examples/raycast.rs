//! Say which block the bot looks at: `block` in chat. Port of mineflayer's
//! `examples/raycast.js`.
//!
//! ```sh
//! cargo run --example raycast -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::{DEFAULT_CURSOR_DISTANCE, prelude::*};

#[tokio::main]
async fn main() {
    let bot = common::connect("raycast", "raycast").await;
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::MessageStr { message, .. } if message.contains("block") => {
                let reply = match bot.block_at_cursor(DEFAULT_CURSOR_DISTANCE, None) {
                    Some(block) => format!("Looking at {}", block.short_name()),
                    None => "Looking at Air".into(),
                };
                let _ = bot.chat(&reply);
            }
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}
