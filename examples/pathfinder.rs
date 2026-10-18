//! Walk to the player who says `come`. Port of mineflayer's
//! `examples/pathfinder/gps.js`.
//!
//! ```sh
//! cargo run --example pathfinder -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;

/// Get within this radius of the player.
const RANGE_GOAL: f32 = 1.;

#[tokio::main]
async fn main() {
    let bot = common::connect("pathfinder", "gps").await;
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username == bot.username() || message != "come" {
            continue;
        }
        let Some(target) = bot.players().get(&username).and_then(|p| p.entity.clone()) else {
            let _ = bot.chat("I don't see you !");
            continue;
        };
        bot.pathfinder().set_movements(Movements::default());
        let goal = Goal::Near {
            pos: target.position(),
            radius: RANGE_GOAL,
        };
        if let Err(err) = bot.pathfinder().set_goal(goal, false) {
            let _ = bot.chat(&err.to_string());
        }
    }
}
