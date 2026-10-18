//! Print the titles the server shows. Port of mineflayer's
//! `examples/titles.js`.
//!
//! ```sh
//! cargo run --example titles -- <host> <port> [<name>]
//! ```
//! Try `/title @a title {"text":"Hello World"}`, `/title @a times 10 20 30`,
//! and `/title @a clear`.

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("titles", "titles").await;
    println!("Bot spawned! Try the /title command.");
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::Title { text, kind } => println!("Received {kind:?}: {text}"),
            BotEvent::TitleTimes {
                fade_in,
                stay,
                fade_out,
            } => println!("Title timing: fadeIn={fade_in}ms, stay={stay}ms, fadeOut={fade_out}ms"),
            BotEvent::TitleClear => println!("All titles cleared"),
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}
