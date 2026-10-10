//! The azalea [`Event`] → [`BotEvent`] translation task.
//!
//! Spawned once per `Bot` by [`BotBuilder::connect`]. This is the pattern
//! every later event source follows: take an azalea event stream, translate
//! to `BotEvent`s, forward over the broadcast channel, and shut the client
//! down when every `Bot` handle is gone.

use std::sync::{Arc, Mutex};

use azalea::{ClientInformation, Event};
use tokio::sync::{broadcast, mpsc::UnboundedReceiver, watch};

use crate::BotEvent;
use crate::entity;

/// Spawn the bridge task. The task ends — and calls `client.exit()` — when
/// azalea's event channel closes (disconnect/shutdown) or when the last
/// `Bot` handle drops (all `alive` senders gone → `changed()` errors).
pub(crate) fn spawn(
    client: azalea::Client,
    mut rx: UnboundedReceiver<Event>,
    events: broadcast::Sender<BotEvent>,
    view_distance: Option<u8>,
    quit_reason: Arc<Mutex<Option<String>>>,
    mut alive: watch::Receiver<()>,
    entity_tracker: Arc<std::sync::RwLock<entity::EntityTracker>>,
) {
    tokio::spawn(async move {
        // Mark the initial watch value as seen so `changed()` below only
        // resolves when the channel closes (all `Bot`s dropped).
        let _ = alive.borrow_and_update();
        let mut state = BridgeState::default();
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
                    // Entity-domain decode runs before translate borrows the
                    // event; its events are emitted right after the raw
                    // `Packet`/`Spawn` they were derived from.
                    let derived = entity::observe_event(&entity_tracker, &client, &event);
                    for bot_event in translate(&mut state, &quit_reason, event) {
                        // SendError = no subscribers right now; that's fine,
                        // receivers may subscribe later.
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
        Event::Chat(packet) => vec![BotEvent::Chat(packet)],
        Event::Tick => vec![BotEvent::Tick],
        Event::Packet(packet) => vec![BotEvent::Packet(packet)],
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
}
