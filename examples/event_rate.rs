//! Measurement probe for plan items G1/C2: `BotEvent` volume per second,
//! `find_blocks` with a predicate, and `Bot::entities()` with a crowd.
//!
//! ```sh
//! cargo run --release --example event_rate -- <host> <port> <name> [idle_s] [busy_s]
//! ```
//!
//! The bot must be op on the server: the busy phase summons 200 chickens
//! with `/summon` and removes them with `/kill` at the end.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    time::{Duration, Instant},
};

use mineflayer::prelude::*;
use tokio::sync::broadcast::error::RecvError;

/// Keeps the `Debug` text up to the first non-identifier char, then stops
/// the formatter — the variant name without formatting a whole packet.
struct VariantName(String);

impl std::fmt::Write for VariantName {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        for c in s.chars() {
            if c.is_alphanumeric() || c == '_' {
                self.0.push(c);
            } else {
                return Err(std::fmt::Error);
            }
        }
        Ok(())
    }
}

fn variant(event: &BotEvent) -> String {
    let mut name = VariantName(String::new());
    let _ = write!(name, "{event:?}");
    name.0
}

#[derive(Default)]
struct Counts {
    total: BTreeMap<String, u64>,
    per_second: Vec<u64>,
    lagged: u64,
}

async fn count(bot: &Bot, seconds: u64) -> Counts {
    let mut events = bot.events();
    let mut counts = Counts::default();
    let start = Instant::now();
    let mut second = 0u64;
    let mut in_second = 0u64;
    while start.elapsed() < Duration::from_secs(seconds) {
        let left = Duration::from_secs(seconds).saturating_sub(start.elapsed());
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Ok(event)) => {
                let now = start.elapsed().as_secs();
                while second < now {
                    counts.per_second.push(in_second);
                    in_second = 0;
                    second += 1;
                }
                in_second += 1;
                *counts.total.entry(variant(&event)).or_default() += 1;
            }
            Ok(Err(RecvError::Lagged(n))) => counts.lagged += n,
            Ok(Err(RecvError::Closed)) | Err(_) => break,
        }
    }
    counts.per_second.push(in_second);
    counts
}

fn report(label: &str, counts: &Counts, seconds: u64) {
    let total: u64 = counts.total.values().sum();
    let peak = counts.per_second.iter().copied().max().unwrap_or(0);
    println!(
        "== {label}: {total} events in {seconds}s, mean {:.1}/s, peak {peak}/s, lagged {}",
        total as f64 / seconds as f64,
        counts.lagged
    );
    let mut top: Vec<_> = counts.total.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    for (name, n) in top.into_iter().take(12) {
        println!("   {name:<28} {n}");
    }
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("Usage: event_rate <host> <port> <name> [idle_s] [busy_s]");
        std::process::exit(1);
    }
    let idle: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(120);
    let busy: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(180);
    let bot = Bot::builder()
        .host(&args[1])
        .port(args[2].parse().expect("port must be a number"))
        .username(&args[3])
        .connect()
        .await
        .expect("connect failed");
    bot.wait_for(Duration::from_secs(30), |e| matches!(e, BotEvent::Spawn))
        .await
        .expect("spawn");
    bot.wait_for_chunks_to_load().await.expect("chunks");
    println!("spawned at {:?}", bot.position());

    // 1. idle event volume
    let idle_counts = count(&bot, idle).await;
    report("idle", &idle_counts, idle);

    // 2. busy event volume: 200 chickens around the bot
    for _ in 0..200 {
        let _ = bot.chat("/summon minecraft:chicken ~ ~1 ~");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let busy_counts = count(&bot, busy).await;
    report("busy (200 chickens)", &busy_counts, busy);

    // 3. Bot::entities() with the crowd
    let n = bot.entities().len();
    let samples: Vec<Duration> = (0..100)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(bot.entities());
            t.elapsed()
        })
        .collect();
    println!(
        "== entities(): {n} entities, median {:?} over 100 calls",
        median(samples)
    );

    // 4. find_blocks with a predicate, max_distance 64
    let samples: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let found = bot.find_blocks(
                FindBlocksOpts::new(|b: &mineflayer::Block| b.kind() == BlockKind::DiamondOre)
                    .max_distance(64.0)
                    .count(10),
            );
            std::hint::black_box(found);
            t.elapsed()
        })
        .collect();
    println!(
        "== find_blocks(predicate, 64): median {:?} over 5 calls",
        median(samples)
    );

    let _ = bot.chat("/kill @e[type=minecraft:chicken]");
    tokio::time::sleep(Duration::from_secs(1)).await;
    bot.quit(None);
}
