//! The simplest bot: echoes back everything said on chat.
//! Port of mineflayer's `examples/echo.js`.
//!
//! ```sh
//! cargo run --example echo -- <host> <port> [<name>] [online]
//! ```

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    // Diagnostics go through `tracing`; the crate never installs a
    // subscriber itself. RUST_LOG=debug (or mineflayer=debug) turns on
    // the bridge's debug logs — dropped events, decode failures, the
    // keepalive timeout, and the bridge exit reason.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(true)
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 5 {
        eprintln!("Usage: cargo run --example echo -- <host> <port> [<name>] [online]");
        std::process::exit(1);
    }
    let username = args.get(3).cloned().unwrap_or_else(|| "echo".to_string());
    // Upstream's `[online?]` flag selects Microsoft auth (username = account
    // email); absent means an offline-mode account.
    let auth = if args.len() > 4 {
        Auth::Microsoft {
            email: username.clone(),
            on_msa_code: None,
        }
    } else {
        Auth::Offline {
            username: username.clone(),
        }
    };

    let bot = Bot::builder()
        .host(&args[1])
        .port(args[2].parse().expect("port must be a number"))
        .auth(auth)
        .connect()
        .await
        .expect("connect failed");

    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            // `username()` resolves once the profile lands — pre-spawn
            // it's empty.
            BotEvent::Spawn => println!("spawned as {}", bot.username()),
            // Upstream's `chat` event carries (username, message); the raw
            // ChatPacket's split_sender_and_content is the same pair.
            // `None` sender = system message, which upstream never emits
            // on `chat`.
            BotEvent::Chat(packet) => {
                let (Some(sender), content) = packet.split_sender_and_content() else {
                    continue;
                };
                if sender == bot.username() {
                    continue;
                }
                println!("{sender}: {content}");
                if let Err(err) = bot.chat(&content) {
                    eprintln!("echo failed: {err}");
                }
            }
            BotEvent::Kicked { reason, .. } => {
                println!("I was kicked from the server: {reason}")
            }
            BotEvent::End(reason) => {
                println!("disconnected: {reason}");
                break;
            }
            _ => {}
        }
    }
}
