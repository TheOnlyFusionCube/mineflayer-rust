//! Attack the speaker or the nearest entity on a chat command. Port of
//! mineflayer's `examples/attack.js`.
//!
//! ```sh
//! cargo run --example attack -- <host> <port> [<name>]
//! ```
//! Chat: `attack me`, `attack`.

mod common;

use futures::StreamExt;

#[tokio::main]
async fn main() {
    let bot = common::connect("attack", "attack").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        match message.as_str() {
            "attack me" => match bot.players().get(&username).and_then(|p| p.entity.clone()) {
                Some(player) => {
                    let _ = bot.chat(&format!("Attacking {username}"));
                    bot.attack(&player, true);
                }
                None => {
                    let _ = bot.chat("I can't see you");
                }
            },
            "attack" => match bot.nearest_entity(|_| true) {
                Some(entity) => {
                    let _ = bot.chat("Attacking the nearest entity");
                    bot.attack(&entity, true);
                }
                None => {
                    let _ = bot.chat("No nearby entities");
                }
            },
            _ => {}
        }
    }
}
