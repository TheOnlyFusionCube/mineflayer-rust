use std::sync::Arc;

use tokio::sync::broadcast;

use super::*;

fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}

/// A caller that drops `tab_complete` before the reply must not leave
/// its correlation entry behind.
#[tokio::test]
async fn cancelled_tab_complete_leaves_no_pending_entry() {
    let client = crate::test_util::live_entity_client((
        mineflayer_engine::world::WorldName(mineflayer_engine::Identifier::new(
            "minecraft:overworld",
        )),
        ChatState::default(),
    ));
    let bot = crate::test_util::bot(client);
    let call = bot.tab_complete("/he");
    // poll once so the entry is registered, then cancel
    let cancelled = tokio::time::timeout(Duration::from_millis(10), call).await;
    assert!(cancelled.is_err(), "no server, so no reply arrives");
    assert_eq!(
        crate::bot::with_component::<ChatState, _>(&bot.client, |c| c.completion_pending.len()),
        Some(0)
    );
}

#[test]
fn split_short_message() {
    assert_eq!(
        split_chat_message("", "hello", CHAT_LENGTH_LIMIT),
        vec!["hello"]
    );
}

#[test]
fn split_command_never_splits() {
    let cmd = format!("/{}", "x".repeat(300));
    assert_eq!(split_chat_message("", &cmd, CHAT_LENGTH_LIMIT), vec![cmd]);
}

#[test]
fn split_long_message() {
    let msg = "x".repeat(300);
    let parts = split_chat_message("", &msg, CHAT_LENGTH_LIMIT);
    assert_eq!(parts, vec!["x".repeat(256), "x".repeat(44)]);
}

#[test]
fn split_boundary_exactly_limit() {
    assert_eq!(
        split_chat_message("", &"x".repeat(256), CHAT_LENGTH_LIMIT),
        vec!["x".repeat(256)]
    );
    assert_eq!(
        split_chat_message("", &"x".repeat(257), CHAT_LENGTH_LIMIT),
        vec!["x".repeat(256), "x".to_string()]
    );
}

#[test]
fn split_skips_empty_lines() {
    assert_eq!(
        split_chat_message("", "a\n\nb\n", CHAT_LENGTH_LIMIT),
        vec!["a".to_string(), "b".to_string()]
    );
    assert!(split_chat_message("", "", CHAT_LENGTH_LIMIT).is_empty());
}

#[test]
fn split_respects_custom_chat_length_limit() {
    // options.chatLengthLimit: a 100-char message at limit 30 splits 4x
    let msg = "x".repeat(100);
    let parts = split_chat_message("", &msg, 30);
    assert_eq!(
        parts,
        vec![
            "x".repeat(30),
            "x".repeat(30),
            "x".repeat(30),
            "x".repeat(10)
        ]
    );
    // the header counts against the custom limit too
    let parts = split_chat_message("/tell bot ", &"y".repeat(15), 12);
    assert_eq!(
        parts,
        vec![
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            format!("/tell bot {}", "y".repeat(2)),
            "/tell bot y".to_string(),
        ]
    );
    // commands still never split
    let cmd = format!("/{}", "x".repeat(50));
    assert_eq!(split_chat_message("", &cmd, 10), vec![cmd]);
}

#[test]
fn split_with_header_counts_against_limit() {
    // "/tell u2bot " is 12 chars -> 244 per chunk
    let msg = "x".repeat(300);
    let parts = split_chat_message("/tell u2bot ", &msg, CHAT_LENGTH_LIMIT);
    assert_eq!(
        parts,
        vec![
            format!("/tell u2bot {}", "x".repeat(244)),
            format!("/tell u2bot {}", "x".repeat(56)),
        ]
    );
}

#[test]
fn pattern_single_match_emits_messages() {
    let mut p = ChatPatterns::default();
    p.add(
        "greet",
        vec![re("^hello (\\w+)$")],
        ChatPatternOpts::default(),
    );
    let events = p.handle_message("hello bob");
    assert!(
        matches!(&events[..], [BotEvent::ChatPattern { name, matches: ChatPatternMatch::Messages(m) }]
                if name == "greet" && m == &vec!["hello bob".to_string()])
    );
}

#[test]
fn pattern_parse_emits_capture_groups() {
    let mut p = ChatPatterns::default();
    p.add(
        "greet",
        vec![re("^hello (\\w+)(!)?$")],
        ChatPatternOpts {
            repeat: true,
            parse: true,
        },
    );
    let events = p.handle_message("hello bob");
    assert!(
        matches!(&events[..], [BotEvent::ChatPattern { name, matches: ChatPatternMatch::Groups(g) }]
                if name == "greet" && g == &vec![vec![Some("bob".to_string()), None]])
    );
}

#[test]
fn pattern_set_needs_consecutive_matches() {
    let mut p = ChatPatterns::default();
    p.add(
        "pair",
        vec![re("^first$"), re("^second$")],
        ChatPatternOpts::default(),
    );
    assert!(p.handle_message("unrelated").is_empty());
    assert!(p.handle_message("first").is_empty()); // position advances
    assert!(p.handle_message("unrelated").is_empty()); // stays at idx 1
    let events = p.handle_message("second");
    assert!(
        matches!(&events[..], [BotEvent::ChatPattern { matches: ChatPatternMatch::Messages(m), .. }]
                if m == &vec!["first".to_string(), "second".to_string()])
    );
    // repeat: re-armed for the next sequence
    assert!(p.handle_message("second").is_empty()); // position reset to 0
    assert!(p.handle_message("first").is_empty());
    assert!(!p.handle_message("second").is_empty());
}

#[test]
fn pattern_no_repeat_removes() {
    let mut p = ChatPatterns::default();
    let id = p.add(
        "once",
        vec![re("^hi$")],
        ChatPatternOpts {
            repeat: false,
            parse: false,
        },
    );
    assert_eq!(p.handle_message("hi").len(), 1);
    assert!(!p.entries.contains_key(&id));
    assert!(p.handle_message("hi").is_empty());
}

#[test]
fn pattern_remove_by_id_and_name() {
    let mut p = ChatPatterns::default();
    let a = p.add("x", vec![re("a")], ChatPatternOpts::default());
    let _b = p.add("x", vec![re("b")], ChatPatternOpts::default());
    p.remove(&ChatPatternKey::Id(a));
    assert_eq!(p.list().len(), 1);
    p.remove(&ChatPatternKey::Name("x".to_string()));
    assert!(p.list().is_empty());
}

#[test]
fn empty_pattern_set_never_matches() {
    // an empty set must not panic the bridge-side matcher
    let mut shared = ChatState::default();
    shared
        .patterns
        .add("empty", vec![], ChatPatternOpts::default());
    let events = shared.message_events(&ChatPacket::new("anything"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, BotEvent::ChatPattern { .. }))
    );
    assert_eq!(shared.patterns.list().len(), 1);
}

#[test]
fn matcher_variants() {
    assert!(MessageMatcher::from("hi").matches("hi"));
    assert!(!MessageMatcher::from("hi").matches("oh hi"));
    assert!(MessageMatcher::from(vec!["a", "b"]).matches("b"));
    assert!(!MessageMatcher::from(vec!["a", "b"]).matches("c"));
    assert!(MessageMatcher::from(re("^<\\w+>")).matches("<bob> yo"));
}

#[tokio::test]
async fn await_message_resolves_on_match() {
    let (tx, rx) = broadcast::channel(16);
    let matchers = [MessageMatcher::from("secret")];
    let wait = await_message_on(rx, &matchers);
    tx.send(BotEvent::MessageStr {
        message: "other".to_string(),
        position: MessagePosition::Chat,
        packet: ChatPacket::new("other"),
    })
    .unwrap();
    tx.send(BotEvent::MessageStr {
        message: "secret".to_string(),
        position: MessagePosition::Chat,
        packet: ChatPacket::new("secret"),
    })
    .unwrap();
    assert_eq!(wait.await.unwrap(), "secret");
}

#[tokio::test]
async fn await_message_errors_on_closed_stream() {
    let (tx, rx) = broadcast::channel::<BotEvent>(16);
    drop(tx);
    assert!(matches!(
        await_message_on(rx, &[MessageMatcher::from("x")]).await,
        Err(Error::NotConnected)
    ));
}

#[test]
fn message_events_system_chat() {
    let mut shared = ChatState::default();
    let events = shared.message_events(&ChatPacket::new("server motd"));
    assert!(matches!(
        &events[..],
        [
            BotEvent::Message {
                position: MessagePosition::System,
                ..
            },
            BotEvent::MessageStr { message, .. }
        ] if message == "server motd"
    ));
}

#[test]
fn message_events_whisper() {
    let mut shared = ChatState::default();
    let events = shared.message_events(&ChatPacket::new("bob whispers: psst"));
    assert!(
        events.iter().any(|e| matches!(e, BotEvent::Whisper(_))),
        "whisper text should emit BotEvent::Whisper: {events:?}"
    );
}

#[test]
fn message_events_whisper_gated_by_default_chat_patterns() {
    // options.defaultChatPatterns: false — upstream never registers the
    // legacy whisper patterns, so no whisper event; message/messagestr
    // still fire (they come from packet listeners, not patterns).
    let mut shared = ChatState {
        default_chat_patterns: false,
        ..ChatState::default()
    };
    let events = shared.message_events(&ChatPacket::new("bob whispers: psst"));
    assert!(
        !events.iter().any(|e| matches!(e, BotEvent::Whisper(_))),
        "defaultChatPatterns=false should suppress Whisper: {events:?}"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        BotEvent::MessageStr { message, .. } if message == "bob whispers: psst"
    )));
}

#[test]
fn message_events_game_info_gets_action_bar() {
    let mut shared = ChatState::default();
    let packet = ChatPacket::System(Arc::new(
        mineflayer_engine::protocol::packets::game::c_system_chat::ClientboundSystemChat {
            content: FormattedText::from("action!"),
            overlay: true,
        },
    ));
    let events = shared.message_events(&packet);
    assert!(matches!(
        &events[..],
        [
            BotEvent::Message {
                position: MessagePosition::GameInfo,
                ..
            },
            BotEvent::MessageStr { .. },
            BotEvent::ActionBar(_)
        ]
    ));
}

#[test]
fn message_events_runs_patterns_on_messagestr() {
    let mut shared = ChatState::default();
    shared
        .patterns
        .add("shout", vec![re("LOUD")], ChatPatternOpts::default());
    let events = shared.message_events(&ChatPacket::new("very LOUD noise"));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, BotEvent::ChatPattern { name, .. } if name == "shout"))
    );
}

#[test]
fn completion_reply_resolves_pending() {
    let mut shared = ChatState::default();
    let (id, mut rx) = shared.register_completion();
    assert_eq!(id, 0);
    shared.complete_suggestions(id, vec!["gamemode".to_string()]);
    assert_eq!(rx.try_recv().unwrap(), vec!["gamemode".to_string()]);
    // an unregistered id is ignored
    shared.complete_suggestions(99, vec!["x".to_string()]);
}

#[test]
fn tablist_stores_and_defaults_empty() {
    let mut shared = ChatState::default();
    assert_eq!(shared.tablist.header.to_string(), "");
    shared.tablist = TabList {
        header: FormattedText::from("h"),
        footer: FormattedText::from("f"),
    };
    assert_eq!(shared.tablist.header.to_string(), "h");
    assert_eq!(shared.tablist.footer.to_string(), "f");
}

#[test]
fn chat_message_parses_json_and_accessors() {
    // text + extra: upstream `new ChatMessage({text, extra})`
    let msg = ChatMessage::parse(r#"{"text":"hi","extra":[{"text":"!"},{"text":"?"}]}"#).unwrap();
    assert_eq!(msg.to_string(), "hi!?");
    assert_eq!(msg.text(), Some("hi"));
    assert_eq!(msg.extra().len(), 2);

    // bare strings parse as text components (upstream does the same)
    let msg = ChatMessage::parse("\"plain\"").unwrap();
    assert_eq!(msg.to_string(), "plain");

    // translatable: key + args, rendered through en_us like upstream's
    // default toString()
    let msg =
        ChatMessage::parse(r#"{"translate":"chat.type.text","with":["faye","hello"]}"#).unwrap();
    assert_eq!(msg.translate(), Some("chat.type.text"));
    assert_eq!(msg.with().map(|a| a.len()), Some(2));
    assert_eq!(msg.to_string(), "<faye> hello");
    assert!(msg.text().is_none());

    // score/selector/keybind components parse and render vanilla's
    // fallback text, like prismarine-chat
    for (json, text) in [
        (r#"{"score":{"name":"a","objective":"b","value":"3"}}"#, "3"),
        (r#"{"selector":"@a"}"#, "@a"),
        (r#"{"keybind":"key.jump"}"#, "key.jump"),
    ] {
        assert_eq!(
            ChatMessage::parse(json).unwrap().to_string(),
            text,
            "{json}"
        );
    }
}

#[test]
fn chat_message_from_notch_parses_section_codes() {
    let msg = ChatMessage::from_notch("§bhi§r!");
    assert_eq!(msg.to_string(), "hi!");
    // §b styles the run — the ansi render carries escape codes
    assert_ne!(msg.to_ansi(), msg.to_string());
}
