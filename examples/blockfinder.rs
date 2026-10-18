//! Find the ten nearest blocks of a kind and say how long the search took.
//! Port of mineflayer's `examples/blockfinder.js`.
//!
//! ```sh
//! cargo run --example blockfinder -- <host> <port> [<name>]
//! ```
//! Chat: `loaded`, `find <block>`.

mod common;

use std::time::Instant;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("blockfinder", "finder").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() {
            continue;
        }
        if message == "loaded" {
            println!("{:?}", bot.position());
            if bot.wait_for_chunks_to_load().await.is_ok() {
                let _ = bot.chat("Ready!");
            }
        } else if let Some(name) = message.strip_prefix("find ") {
            let Ok(kind) = name.parse::<BlockKind>() else {
                let _ = bot.chat(&format!("{name} is not a block name"));
                continue;
            };
            let start = Instant::now();
            let blocks = bot.find_blocks(FindBlocksOpts::new(kind).max_distance(128.).count(10));
            let ms = start.elapsed().as_secs_f64() * 1000.;
            let _ = bot.chat(&format!(
                "I found {} {name} blocks in {ms:.2} ms",
                blocks.len()
            ));
        }
    }
}
