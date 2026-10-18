//! Resource probe: connect `<count>` offline-mode bots one after another
//! (each spawns, then 100 ms pass), keep
//! them idle for `<seconds>` after the last spawn, then quit. Run it under
//! `/usr/bin/time -l` (macOS) or `/usr/bin/time -v` (Linux) for the peak
//! memory and the CPU time; `scripts/bench/idle_bots.js` is the same probe
//! for upstream mineflayer (docs/performance.md).
//!
//! ```sh
//! cargo build --release --example idle_bots
//! /usr/bin/time -l target/release/examples/idle_bots localhost 25566 10 60
//! ```

use std::time::{Duration, Instant};

use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("Usage: idle_bots <host> <port> <count> <seconds>");
        std::process::exit(1);
    }
    let port: u16 = args[2].parse().expect("the port is a number");
    let count: usize = args[3].parse().expect("the count is a number");
    let idle = Duration::from_secs(args[4].parse().expect("the seconds are a number"));

    let start = Instant::now();
    let mut bots = Vec::new();
    for i in 0..count {
        let bot = Bot::builder()
            .host(&args[1])
            .port(port)
            .username(format!("idle{i}"))
            .view_distance(4)
            .packet_events(false)
            .connect()
            .await
            .expect("connect failed");
        bot.spawned().await.expect("the bot spawned");
        bots.push(bot);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!(
        "{count} bots spawned in {:.1} s",
        start.elapsed().as_secs_f64()
    );
    // the event streams stay subscribed and drained, as a bot's handler
    // would keep them
    let drains: Vec<_> = bots
        .iter()
        .map(|bot| {
            let mut events = bot.events();
            tokio::spawn(async move { while events.recv().await.is_ok() {} })
        })
        .collect();
    tokio::time::sleep(idle).await;
    for bot in &bots {
        bot.quit(None);
    }
    for drain in drains {
        drain.abort();
    }
    println!("idle for {} s", idle.as_secs());
}
