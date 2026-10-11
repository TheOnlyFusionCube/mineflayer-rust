//! The azalea [`Event`] → [`BotEvent`] translation task.
//!
//! Spawned once per `Bot` by [`BotBuilder::connect`]. This is the pattern
//! every later event source follows: take an azalea event stream, translate
//! to `BotEvent`s, forward over the broadcast channel, and shut the client
//! down when every `Bot` handle is gone.

use std::sync::{Arc, Mutex};

use azalea::{
    ClientInformation, Event,
    chat::ChatPacket,
    protocol::packets::game::{ClientboundGamePacket, c_system_chat::ClientboundSystemChat},
};
use tokio::sync::{broadcast, mpsc::UnboundedReceiver, watch};

use crate::{
    BotEvent,
    chat::{ChatShared, TabList},
    entity,
};

/// Shared state the bridge needs beyond its own bookkeeping — one struct so
/// `spawn` stays under `clippy::too_many_arguments` as domains add theirs.
pub(crate) struct BridgeDeps {
    /// `Bot::quit`/`end` reason, consumed by `translate` on `End`.
    pub quit_reason: Arc<Mutex<Option<String>>>,
    /// Chat-domain state (pattern registry, tablist, `tab_complete` correlation).
    pub chat: Arc<Mutex<ChatShared>>,
    /// Entity-domain packet decode state — see [`entity::EntityTracker`].
    pub entities: Arc<std::sync::RwLock<entity::EntityTracker>>,
}

/// Spawn the bridge task. The task ends — and calls `client.exit()` — when
/// azalea's event channel closes (disconnect/shutdown) or when the last
/// `Bot` handle drops (all `alive` senders gone → `changed()` errors).
pub(crate) fn spawn(
    client: azalea::Client,
    mut rx: UnboundedReceiver<Event>,
    events: broadcast::Sender<BotEvent>,
    view_distance: Option<u8>,
    mut alive: watch::Receiver<()>,
    deps: BridgeDeps,
) {
    tokio::spawn(async move {
        let BridgeDeps {
            quit_reason,
            chat,
            entities,
        } = deps;
        // Mark the initial watch value as seen so `changed()` below only
        // resolves when the channel closes (all `Bot`s dropped).
        let _ = alive.borrow_and_update();
        let mut state = BridgeState {
            chat,
            ..Default::default()
        };
        loop {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    // Client information must be set on Init so the login
                    // packet already carries it (azalea docs).
                    if let (Event::Init, Some(vd)) = (&event, view_distance) {
                        client.set_client_information(ClientInformation {
                            view_distance: vd,
                            ..Default::default()
                        });
                    }
                    // Domain decodes azalea doesn't lift into `Event`s
                    // (movement: move diff/forcedMove/abilities; entities:
                    // the whole entity event family). Both run before
                    // translate borrows the event; their events are emitted
                    // right after the raw `Packet`/`Spawn` they came from.
                    let movement_events =
                        crate::movement::bridge_events(&client, &mut state.movement, &event);
                    let pathfinder_events = crate::pathfinder::bridge_events(
                        &client,
                        &mut state.pathfinder,
                        &event,
                    );
                    let derived = entity::observe_event(&entities, &client, &event);
                    for bot_event in translate(&mut state, &quit_reason, event) {
                        // SendError = no subscribers right now; that's fine,
                        // receivers may subscribe later.
                        let _ = events.send(bot_event.clone());
                        // misc domains (scoreboards, sounds, …) decode Packet
                        // events into facade state + their own BotEvents,
                        // emitted after the source event itself.
                        crate::misc::after_event(&client, &bot_event, &events);
                    }
                    for bot_event in movement_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in pathfinder_events {
                        let _ = events.send(bot_event);
                    }
                    for bot_event in derived {
                        let _ = events.send(bot_event);
                    }
                }
                _ = alive.changed() => break,
            }
        }
        // azalea stopped feeding events or all `Bot` handles are gone —
        // shut the client down so the ECS runner doesn't leak.
        client.exit();
    });
}

/// Bridge bookkeeping that doesn't fit in the events themselves.
#[derive(Default)]
struct BridgeState {
    /// Whether `Spawn` has been emitted yet. Subsequent azalea `Spawn`s are
    /// respawns/dimension changes; mineflayer reports those as
    /// `respawn` followed by `spawn`.
    seen_spawn: bool,
    /// Whether `End` has been emitted — dedupes disconnect signals so `end`
    /// fires exactly once, like upstream.
    ended: bool,
    /// Movement tracking for `move`/`mount`/`dismount` (U5).
    movement: crate::movement::MovementTracker,
    /// Chat-domain state shared with `Bot` (pattern registry, tablist,
    /// `tab_complete` correlation).
    chat: Arc<Mutex<ChatShared>>,
    /// Pathfinder component-diff state for the goal/path events (U7).
    pathfinder: crate::pathfinder::PathTracker,
}

/// Translate one azalea event into zero or more `BotEvent`s, in emission
/// order.
fn translate(
    state: &mut BridgeState,
    quit_reason: &Mutex<Option<String>>,
    event: Event,
) -> Vec<BotEvent> {
    match event {
        Event::Init => vec![BotEvent::Init],
        Event::Login => vec![BotEvent::Login],
        Event::Spawn => {
            if state.seen_spawn {
                vec![BotEvent::Respawn, BotEvent::Spawn]
            } else {
                state.seen_spawn = true;
                vec![BotEvent::Spawn]
            }
        }
        Event::Death(_) => vec![BotEvent::Death],
        Event::Disconnect(reason) => {
            if state.ended {
                return vec![];
            }
            state.ended = true;
            match reason {
                // azalea attaches a reason iff the server kicked us
                // (`DisconnectEvent` docs); `None` means local/socket close.
                Some(reason) => {
                    let reason = reason.to_string();
                    vec![BotEvent::Kicked(reason.clone()), BotEvent::End(reason)]
                }
                None => {
                    let reason = quit_reason
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap_or_else(|| "socketClosed".to_string());
                    vec![BotEvent::End(reason)]
                }
            }
        }
        Event::ConnectionFailed(error) => {
            if state.ended {
                return vec![];
            }
            state.ended = true;
            // upstream emits 'error' then 'end' on a failed connect
            vec![
                BotEvent::Error(error.to_string()),
                BotEvent::End("socketClosed".to_string()),
            ]
        }
        Event::Chat(packet) => {
            let mut out = vec![BotEvent::Chat(packet.clone())];
            out.extend(state.chat.lock().unwrap().message_events(&packet));
            out
        }
        Event::Tick => vec![BotEvent::Tick],
        Event::Packet(packet) => {
            let mut out = vec![BotEvent::Packet(packet.clone())];
            out.extend(packet_side_events(&state.chat, &packet));
            out
        }
        Event::AddPlayer(info) => vec![BotEvent::PlayerJoined(info)],
        Event::UpdatePlayer(info) => vec![BotEvent::PlayerUpdated(info)],
        Event::RemovePlayer(info) => vec![BotEvent::PlayerLeft(info)],
        Event::ReceiveChunk(pos) => vec![BotEvent::ChunkColumnLoad(pos)],
        // KeepAlive has no mineflayer analog — azalea answers keepalives
        // itself. Dropped by design (see api-parity.md `options.keepAlive`).
        Event::KeepAlive(_) => vec![],
        // azalea's Event is #[non_exhaustive]: anything it adds upstream in
        // the future is dropped rather than crashing the bridge. (No logging
        // facade is in our dependency set, so there is nothing to log to.)
        _ => vec![],
    }
}

/// Extra `BotEvent`s decoded from raw clientbound packets azalea's own
/// handlers drop (chat domain): the dedicated action-bar packet feeds the
/// same message family as a chat packet, `c_tab_list` updates the stored
/// header/footer, and `c_command_suggestions` resolves a `tab_complete` call.
/// The raw packet itself still goes out as `BotEvent::Packet` first.
fn packet_side_events(chat: &Mutex<ChatShared>, packet: &ClientboundGamePacket) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::SetActionBarText(p) => {
            // upstream routes game_info text through
            // message/messagestr/actionBar, so synthesize the overlay chat
            // packet it would have arrived as on older versions.
            let chat_packet = ChatPacket::System(Arc::new(ClientboundSystemChat {
                content: p.text.clone(),
                overlay: true,
            }));
            chat.lock().unwrap().message_events(&chat_packet)
        }
        ClientboundGamePacket::TabList(p) => {
            chat.lock().unwrap().store_tablist(TabList {
                header: p.header.clone(),
                footer: p.footer.clone(),
            });
            vec![]
        }
        ClientboundGamePacket::CommandSuggestions(p) => {
            let matches = p.suggestions.list().iter().map(|s| s.text()).collect();
            chat.lock().unwrap().complete_suggestions(p.id, matches);
            vec![]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use azalea::protocol::connect::ConnectionError;

    use super::*;

    #[test]
    fn translate_one_to_one() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        assert!(matches!(
            translate(&mut s, &qr, Event::Init)[..],
            [BotEvent::Init]
        ));
        assert!(matches!(
            translate(&mut s, &qr, Event::Login)[..],
            [BotEvent::Login]
        ));
        assert!(matches!(
            translate(&mut s, &qr, Event::Tick)[..],
            [BotEvent::Tick]
        ));
        assert!(matches!(
            translate(&mut s, &qr, Event::Death(None))[..],
            [BotEvent::Death]
        ));
        assert!(matches!(
            translate(
                &mut s,
                &qr,
                Event::ReceiveChunk(azalea::core::position::ChunkPos::new(1, 2))
            )[..],
            [BotEvent::ChunkColumnLoad(_)]
        ));
    }

    #[test]
    fn translate_spawn_then_respawn() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        // first spawn: spawn only
        assert!(matches!(
            translate(&mut s, &qr, Event::Spawn)[..],
            [BotEvent::Spawn]
        ));
        // later spawns (death, dimension change): respawn then spawn
        assert!(matches!(
            translate(&mut s, &qr, Event::Spawn)[..],
            [BotEvent::Respawn, BotEvent::Spawn]
        ));
    }

    #[test]
    fn translate_disconnect_variants() {
        let qr = Mutex::new(None);

        // plain close -> End("socketClosed"), upstream's default reason
        let mut s = BridgeState::default();
        assert!(matches!(
            &translate(&mut s, &qr, Event::Disconnect(None))[..],
            [BotEvent::End(r)] if r == "socketClosed"
        ));
        // End only fires once
        assert!(translate(&mut s, &qr, Event::Disconnect(None)).is_empty());

        // kick -> Kicked then End with the kick reason
        let mut s = BridgeState::default();
        assert!(matches!(
            &translate(
                &mut s,
                &qr,
                Event::Disconnect(Some(azalea::FormattedText::from("banned")))
            )[..],
            [BotEvent::Kicked(k), BotEvent::End(r)] if k == "banned" && r == "banned"
        ));
    }

    #[test]
    fn translate_disconnect_uses_quit_reason() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(Some("disconnect.quitting".to_string()));
        assert!(matches!(
            &translate(&mut s, &qr, Event::Disconnect(None))[..],
            [BotEvent::End(r)] if r == "disconnect.quitting"
        ));
        assert!(qr.lock().unwrap().is_none());
    }

    #[test]
    fn translate_connection_failed_is_error_then_end() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let err = ConnectionError::from(io::Error::other("refused"));
        assert!(matches!(
            &translate(&mut s, &qr, Event::ConnectionFailed(Arc::new(err)))[..],
            [BotEvent::Error(e), BotEvent::End(r)] if e.contains("refused") && r == "socketClosed"
        ));
    }

    #[test]
    fn translate_keepalive_dropped() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        assert!(translate(&mut s, &qr, Event::KeepAlive(42)).is_empty());
    }

    #[test]
    fn translate_chat_emits_message_family() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let out = translate(
            &mut s,
            &qr,
            Event::Chat(azalea::chat::ChatPacket::new("<a> hi")),
        );
        assert!(
            matches!(&out[..],
                [
                    BotEvent::Chat(_),
                    BotEvent::Message { position: crate::MessagePosition::System, .. },
                    BotEvent::MessageStr { message, .. },
                ] if message == "<a> hi"),
            "expected Chat+Message+MessageStr, got {out:?}"
        );
    }

    #[test]
    fn translate_action_bar_packet_feeds_message_family() {
        use azalea::protocol::packets::game::c_set_action_bar_text::ClientboundSetActionBarText;

        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let packet = ClientboundGamePacket::SetActionBarText(ClientboundSetActionBarText {
            text: azalea::FormattedText::from("bar!"),
        });
        let out = translate(&mut s, &qr, Event::Packet(Arc::new(packet)));
        assert!(
            matches!(&out[..],
                [
                    BotEvent::Packet(_),
                    BotEvent::Message { position: crate::MessagePosition::GameInfo, .. },
                    BotEvent::MessageStr { message, .. },
                    BotEvent::ActionBar(_),
                ] if message == "bar!"),
            "expected Packet+Message+MessageStr+ActionBar, got {out:?}"
        );
    }

    #[test]
    fn translate_tab_list_packet_updates_shared() {
        use azalea::protocol::packets::game::c_tab_list::ClientboundTabList;

        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let packet = ClientboundGamePacket::TabList(ClientboundTabList {
            header: azalea::FormattedText::from("h"),
            footer: azalea::FormattedText::from("f"),
        });
        let out = translate(&mut s, &qr, Event::Packet(Arc::new(packet)));
        assert!(matches!(&out[..], [BotEvent::Packet(_)]));
        let tablist = s.chat.lock().unwrap().tablist();
        assert_eq!(tablist.header.to_string(), "h");
        assert_eq!(tablist.footer.to_string(), "f");
    }

    #[test]
    fn translate_command_suggestions_resolves_pending() {
        use azalea::{
            brigadier::{
                context::StringRange,
                suggestion::{Suggestion, Suggestions},
            },
            protocol::packets::game::c_command_suggestions::ClientboundCommandSuggestions,
        };

        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        let (id, mut rx) = s.chat.lock().unwrap().register_completion();
        let packet = ClientboundGamePacket::CommandSuggestions(ClientboundCommandSuggestions {
            id,
            suggestions: Suggestions::new(
                StringRange::new(0, 2),
                vec![Suggestion::new(StringRange::new(0, 2), "gamemode")],
            ),
        });
        let out = translate(&mut s, &qr, Event::Packet(Arc::new(packet)));
        assert!(matches!(&out[..], [BotEvent::Packet(_)]));
        assert_eq!(rx.try_recv().unwrap(), vec!["gamemode".to_string()]);
    }

    #[test]
    fn translate_chat_runs_patterns() {
        let mut s = BridgeState::default();
        let qr = Mutex::new(None);
        s.chat.lock().unwrap().add_pattern(
            "probe",
            vec![regex::Regex::new("worked").unwrap()],
            crate::ChatPatternOpts::default(),
        );
        let out = translate(
            &mut s,
            &qr,
            Event::Chat(azalea::chat::ChatPacket::new("it worked")),
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::ChatPattern { name, .. } if name == "probe")),
            "expected a ChatPattern event, got {out:?}"
        );
    }
}
