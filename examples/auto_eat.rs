//! Eat when hungry with the auto-eat plugin, and stop the monitor at full
//! food. Port of mineflayer's `examples/auto-eat.js`.
//!
//! ```sh
//! cargo run --example auto_eat -- <host> <port> [<name>]
//! ```

mod common;

use futures::StreamExt;
use mineflayer::prelude::*;
use mineflayer_plugins::{
    auto_eat::{AutoEatEvent, FoodPriority},
    prelude::*,
};

#[tokio::main]
async fn main() {
    let bot = common::connect("auto_eat", "autoeat").await;
    let mut opts = bot.auto_eat().opts();
    opts.priority = FoodPriority::FoodPoints;
    opts.min_hunger = 14;
    opts.banned_food.clear();
    bot.auto_eat().set_opts(opts);
    bot.auto_eat().enable_auto();

    let mut eat_events = bot.plugin_events::<AutoEatEvent>();
    let mut events = bot.event_stream();
    loop {
        tokio::select! {
            Ok(event) = eat_events.recv() => match event {
                AutoEatEvent::Start { food, .. } => println!("Auto Eat started! ({})", food.name()),
                AutoEatEvent::Finish { .. } => println!("Auto Eat stopped!"),
                AutoEatEvent::Fail(reason) => println!("Auto Eat failed: {reason}"),
            },
            event = events.next() => match event {
                Some(BotEvent::Health) if bot.food() == 20 => bot.auto_eat().disable_auto(),
                Some(BotEvent::Health) => bot.auto_eat().enable_auto(),
                Some(BotEvent::End(_)) | None => break,
                Some(_) => {}
            },
        }
    }
}
