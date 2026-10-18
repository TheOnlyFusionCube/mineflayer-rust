//! Print the sounds and note blocks the bot hears. Port of mineflayer's
//! `examples/sound.js`.
//!
//! ```sh
//! cargo run --example sound -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let bot = common::connect("sound", "sound").await;
    println!("Bot spawned!");
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::SoundHeard { name, pos, .. } => {
                println!("Heard sound: {name} at {}, {}, {}", pos.x, pos.y, pos.z);
            }
            BotEvent::NoteHeard {
                instrument, pitch, ..
            } => println!("Heard note block: {instrument:?} at pitch {pitch}"),
            BotEvent::End(_) => break,
            _ => {}
        }
    }
}
