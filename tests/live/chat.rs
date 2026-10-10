//! Live chat test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566) and exercises the chat surface
//! against the vanilla server: message echo, patterns, awaitMessage,
//! whisper, tabComplete, and long-message splitting. Only compiled with
//! `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live-chat -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use mineflayer::{Bot, BotEvent, ChatPatternMatch, ChatPatternOpts, MessageMatcher};
use regex::Regex;
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
async fn chat_surface() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u2bot")
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

    // The vanilla server echoes our own chat back; the bridge turns it into
    // the message family. Capture the echo once to learn this server's
    // rendering (sender prefix) rather than assuming "<u2bot> ".
    bot.chat("hello from u2bot").expect("chat failed");
    let echo = wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::MessageStr { message, .. } if message.contains("hello from u2bot")),
        "messagestr echo of our own chat",
    )
    .await;
    let BotEvent::MessageStr { message, .. } = echo else {
        unreachable!()
    };
    let prefix = message
        .strip_suffix("hello from u2bot")
        .expect("echo ends with our text")
        .to_string();
    assert!(
        !prefix.is_empty(),
        "echo should carry a sender prefix, got {message:?}"
    );

    // Chat pattern over the observed echo shape; parse mode captures the body.
    let pattern = format!("^{}(.*)$", regex::escape(&prefix));
    let pattern_id = bot.add_chat_pattern(
        "selfchat",
        Regex::new(&pattern).unwrap(),
        ChatPatternOpts {
            repeat: true,
            parse: true,
        },
    );
    bot.chat("pattern probe").expect("chat failed");
    let event = wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::ChatPattern { name, .. } if name == "selfchat"),
        "chat:selfchat pattern event",
    )
    .await;
    match event {
        BotEvent::ChatPattern {
            matches: ChatPatternMatch::Groups(groups),
            ..
        } => assert_eq!(
            groups,
            vec![vec![Some("pattern probe".to_string())]],
            "capture group should hold the message body"
        ),
        other => panic!("expected ChatPattern Groups, got {other:?}"),
    }
    assert_eq!(bot.chat_patterns().len(), 1);
    bot.remove_chat_pattern(pattern_id);
    assert!(bot.chat_patterns().is_empty());

    // await_message subscribes on first poll; join! lets the subscription
    // land before the chat is sent (50ms guard).
    let (awaited, _) = tokio::join!(
        bot.await_message([MessageMatcher::from(format!("{prefix}await probe"))]),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            bot.chat("await probe").unwrap();
        },
    );
    let msg = awaited.expect("await_message timed out on our own echo");
    assert_eq!(msg, format!("{prefix}await probe"));

    // Whisper to self: the server answers with an incoming-whisper display.
    bot.whisper("u2bot", "psst").expect("whisper failed");
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::Whisper(_)),
        "whisper event",
    )
    .await;

    // Command completion over the tier-2 packet path.
    let matches = bot.tab_complete("/ga").await.expect("tab_complete failed");
    assert!(
        matches.iter().any(|m| m.contains("gamemode")),
        "vanilla should complete /ga to include 'gamemode', got {matches:?}"
    );

    // A 300-char message splits into 256 + 44; both halves echo back.
    bot.chat(&"x".repeat(300)).expect("long chat failed");
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::MessageStr { message, .. } if message.contains(&"x".repeat(256))),
        "first 256-char half of the split echo",
    )
    .await;
    wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::MessageStr { message, .. } if message.contains(&"x".repeat(44)) && !message.contains(&"x".repeat(256))),
        "second 44-char half of the split echo",
    )
    .await;

    // Vanilla sends no tab-list header — the accessor still reads cleanly.
    let tablist = bot.tablist();
    assert_eq!(tablist.header.to_string(), "");
    assert_eq!(tablist.footer.to_string(), "");

    bot.quit(None);
    let end = wait_for(
        &mut rx,
        Duration::from_secs(10),
        |e| matches!(e, BotEvent::End(_)),
        "end",
    )
    .await;
    assert!(matches!(end, BotEvent::End(r) if r == "disconnect.quitting"));
}
