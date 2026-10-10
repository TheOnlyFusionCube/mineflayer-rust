//! Live lifecycle test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566), waits for `spawn`, chats, quits,
//! and expects `end`. Only compiled with `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use mineflayer::{Bot, BotEvent};
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Receive events until one satisfies `pred` (`Lagged` just re-reads).
/// Panics on timeout or a closed stream.
async fn wait_for(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> BotEvent {
    tokio::time::timeout(timeout, async {
        loop {
            match rx.recv().await {
                Ok(event) if pred(&event) => return event,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("event stream closed while waiting for {what}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out after {timeout:?} waiting for {what}"))
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn lifecycle() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u1bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();

    wait_for(
        &mut rx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "spawn",
    )
    .await;
    assert_eq!(bot.username(), "u1bot");
    assert!(bot.is_alive());

    bot.chat("hi").expect("chat failed");
    bot.quit(None);

    let end = wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
    // quit(None) defaults to mineflayer's 'disconnect.quitting'
    assert!(matches!(end, BotEvent::End(r) if r == "disconnect.quitting"));
}
