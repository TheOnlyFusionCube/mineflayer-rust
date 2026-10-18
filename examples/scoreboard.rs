//! Report scoreboards as they are made and their scores as they change.
//! Port of mineflayer's `examples/scoreboard.js`.
//!
//! ```sh
//! cargo run --example scoreboard -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("scoreboard", "scoreboard").await;
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::ScoreboardCreated(scoreboard) => {
                let _ = bot.chat(&format!(
                    "New scoreboard: {}, {}",
                    scoreboard.name, scoreboard.title
                ));
            }
            BotEvent::ScoreUpdated { scoreboard, item } => {
                let _ = bot.chat(&format!(
                    "Scoreboard score : {}, {}, {}",
                    scoreboard.title, item.name, item.value
                ));
            }
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}
