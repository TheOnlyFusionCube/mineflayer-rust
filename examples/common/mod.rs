//! What every example does first: read `<host> <port> [<name>]`,
//! connect an offline-mode bot, and wait for its spawn.

use mineflayer::prelude::*;

/// Connect as `<name>` (default `default_name`) and wait for the spawn.
/// `RUST_LOG` turns on the crate's logs.
pub async fn connect(example: &str, default_name: &str) -> Bot {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let args: Vec<String> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        eprintln!("Usage: cargo run --example {example} -- <host> <port> [<name>]");
        std::process::exit(1);
    }
    let bot = Bot::builder()
        .host(&args[1])
        .port(args[2].parse().expect("the port is a number"))
        .username(args.get(3).map_or(default_name, String::as_str))
        .connect()
        .await
        .expect("connect failed");
    bot.spawned().await.expect("the bot spawned");
    bot
}
