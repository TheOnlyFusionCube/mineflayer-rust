//! The simplest bot: echoes back everything said on chat.
//! Port of mineflayer's `examples/echo.js`.
//!
//! ```sh
//! cargo run --example echo -- <host> <port> [<name>] [online]
//! ```

use mineflayer::{Auth, Bot, BotEvent};

#[tokio::main]
async fn main() {
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

    let mut events = bot.events();
    loop {
        match events.recv().await {
            // `username()` resolves once the profile lands — pre-spawn
            // it's empty.
            Ok(BotEvent::Spawn) => println!("spawned as {}", bot.username()),
            // Upstream's `chat` event carries (username, message); the raw
            // ChatPacket's split_sender_and_content is the same pair.
            // `None` sender = system message, which upstream never emits
            // on `chat`.
            Ok(BotEvent::Chat(packet)) => {
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
            Ok(BotEvent::Kicked(reason)) => println!("I was kicked from the server: {reason}"),
            Ok(BotEvent::End(reason)) => {
                println!("disconnected: {reason}");
                break;
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
