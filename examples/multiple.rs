//! Connect ten bots, 100 ms apart, each on its own thread. Port of
//! mineflayer's `examples/multiple.js`.
//!
//! ```sh
//! cargo run --example multiple -- <host> <port>
//! ```

use std::time::Duration;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: cargo run --example multiple -- <host> <port>");
        std::process::exit(1);
    }
    let port: u16 = args[2].parse().expect("the port is a number");
    let mut bots = Vec::new();
    for i in 1..=10 {
        let bot = Bot::builder()
            .host(&args[1])
            .port(port)
            .username(format!("mineflayer-bot{i}"))
            .connect()
            .await;
        match bot {
            Ok(bot) => bots.push(bot),
            Err(err) => eprintln!("bot {i}: {err}"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("{} bots connected", bots.len());
    // keep them online until every one is gone
    for bot in bots {
        let mut events = bot.event_stream();
        while let Some(event) = events.next().await {
            if let BotEvent::End(reason) = event {
                println!("{}: {reason}", bot.username());
                break;
            }
        }
    }
}
