//! Collect the nearest block of a kind: `collect <block>` in chat. Port of
//! mineflayer's `examples/collectblock.js`.
//!
//! ```sh
//! cargo run --example collectblock -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;
use mineflayer_plugins::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("collectblock", "collector").await;
    let mut chat = bot.chat_messages();
    while let Some((_, message)) = chat.next().await {
        let Some(name) = message.strip_prefix("collect ") else {
            continue;
        };
        let Ok(kind) = name.parse::<BlockKind>() else {
            let _ = bot.chat("I don't know any blocks with that name.");
            continue;
        };
        let _ = bot.chat(&format!("Collecting the nearest {name}"));
        let Some(block) = bot.find_block(FindBlocksOpts::new(kind).max_distance(64.)) else {
            let _ = bot.chat("I don't see that block nearby.");
            continue;
        };
        if let Err(err) = bot.collect_block().collect(block).await {
            let _ = bot.chat(&err.to_string());
        }
    }
}
