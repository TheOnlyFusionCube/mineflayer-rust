//! Ask the server to complete a command: `complete <text>` in chat. Port
//! of mineflayer's `examples/tab_complete.js`.
//!
//! ```sh
//! cargo run --example tab_complete -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("tab_complete", "tab_complete").await;
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::MessageStr { message, .. } => {
                let Some(at) = message.find("complete ") else {
                    continue;
                };
                let text = &message[at + "complete ".len()..];
                match bot.tab_complete(text).await {
                    Ok(matches) => {
                        println!("{text} {matches:?}");
                        let _ =
                            bot.chat(&format!("Matches for \"{text}\": {}", matches.join(", ")));
                    }
                    Err(err) => {
                        let _ = bot.chat(&err.to_string());
                    }
                }
            }
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}
