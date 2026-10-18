//! Make a new bot each time the connection ends. Port of mineflayer's
//! `examples/reconnector.js`: like upstream, the library does not
//! reconnect by itself.
//!
//! ```sh
//! cargo run --example reconnector -- <host> <port> [<name>]
//! ```

use std::time::Duration;

use futures::StreamExt;
use mineflayer::prelude::*;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !(3..=4).contains(&args.len()) {
        eprintln!("Usage: cargo run --example reconnector -- <host> <port> [<name>]");
        std::process::exit(1);
    }
    let port: u16 = args[2].parse().expect("the port is a number");
    let name = args.get(3).map_or("reconnector", String::as_str);
    loop {
        match Bot::builder()
            .host(&args[1])
            .port(port)
            .username(name)
            .connect()
            .await
        {
            Ok(bot) => {
                let mut events = bot.event_stream();
                while let Some(event) = events.next().await {
                    if let BotEvent::End(reason) = event {
                        println!("ended: {reason}");
                        break;
                    }
                }
            }
            Err(err) => println!("{err}"),
        }
        // upstream reconnects at once; a short pause spares the server a
        // busy loop when it refuses
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
