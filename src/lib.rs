//! `mineflayer` is a Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
//! Minecraft bot API. It exposes mineflayer's familiar surface — a `Bot` handle,
//! `bot.on(...)`-style events, and per-domain methods (`chat`, `world`,
//! `inventory`, `pathfinder`, …) — on top of the [`azalea`] crate, which
//! provides the protocol, ECS world state, physics, and pathfinding
//! underneath. The unit of truth for API coverage is `docs/api-parity.md`;
//! every public mineflayer API is tracked there as `pending`, `ported`,
//! `diverges`, or `wontfix`.

mod error;

pub mod chat;
pub mod craft;
pub mod entity;
pub mod interact;
pub mod inventory;
pub mod misc;
pub mod movement;
pub mod pathfinder;
pub mod state;
pub mod windows;
pub mod world;

pub use error::{Error, Result};

use std::sync::Arc;

use azalea::protocol::packets::game::ClientboundGamePacket;
use tokio::sync::broadcast;

/// A connected bot — the Rust analog of the object returned by mineflayer's
/// `createBot()`.
///
/// `Bot` is a cheap cloneable handle wrapping [`azalea::Client`]. High-level
/// mineflayer methods (`bot.chat`, `bot.dig`, `bot.pathfinder`, …) are added
/// to it by later units as extension methods in the domain modules.
///
/// Events are delivered over a [`tokio::sync::broadcast`] channel (the
/// multi-subscriber, lag-tolerant EventEmitter analog): call
/// [`Bot::events`] to subscribe.
#[derive(Clone)]
pub struct Bot {
    client: azalea::Client,
    events: broadcast::Sender<BotEvent>,
}

/// Events emitted by a [`Bot`], mirroring mineflayer's `bot.on(...)` events.
///
/// Bridged from azalea's [`Event`](azalea::Event) enum (plus raw packet
/// decoding for events azalea does not lift). Variant payloads use azalea's
/// types so no data is lost in translation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BotEvent {
    /// `login` — the server accepted our login.
    Login,
    /// `spawn` — the bot spawned (or respawned) into a loaded world.
    Spawn,
    /// `respawn` — a respawn/dimension change is about to happen.
    Respawn,
    /// `death` — the bot died.
    Death,
    /// `kicked` — the server kicked us, with the reason.
    Kicked(String),
    /// `end` — the connection ended, with the reason.
    End(String),
    /// `error` — a non-fatal error was surfaced.
    Error(String),
    /// `chat` / `message` — a chat packet arrived.
    Chat(azalea::chat::ChatPacket),
    /// `physicsTick` — a game tick elapsed while the world was loaded.
    Tick,
    /// Raw clientbound packet escape hatch for APIs azalea doesn't lift
    /// (scoreboards, boss bars, titles, particles, …). Emitted under azalea's
    /// `packet-event` feature, which is on by default.
    Packet(Arc<ClientboundGamePacket>),
}

impl Bot {
    /// Wrap an already-connected [`azalea::Client`] in a `Bot` handle.
    ///
    /// Construction will normally go through `Bot::builder()` /
    /// `Bot::connect()` (a later unit), which also spawns the event-bridge
    /// task that feeds [`Bot::events`].
    #[doc(hidden)]
    pub fn new(client: azalea::Client) -> Self {
        let (events, _) = broadcast::channel(256);
        Self { client, events }
    }

    /// The underlying [`azalea::Client`] — the `bot._client` analog for when
    /// the facade doesn't cover something yet.
    pub fn client(&self) -> &azalea::Client {
        &self.client
    }

    /// Subscribe to this bot's event stream.
    ///
    /// Each call returns an independent receiver; slow consumers see
    /// [`broadcast::error::RecvError::Lagged`] rather than blocking the bot.
    pub fn events(&self) -> broadcast::Receiver<BotEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_event_broadcast_reaches_all_subscribers() {
        let (tx, mut rx1) = broadcast::channel::<BotEvent>(16);
        let mut rx2 = tx.subscribe();

        tx.send(BotEvent::Login).unwrap();
        tx.send(BotEvent::Kicked("bye".to_string())).unwrap();

        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Kicked(_)));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Kicked(_)));
    }

    #[test]
    fn bot_event_payloads_exist() {
        // Proves the event variants the lifecycle bridge needs are real and
        // carry the planned payload types.
        let _ = BotEvent::Spawn;
        let _ = BotEvent::Respawn;
        let _ = BotEvent::Death;
        let _ = BotEvent::End("socketClosed".to_string());
        let _ = BotEvent::Error("boom".to_string());
        let _ = BotEvent::Tick;
        let _ = |p: azalea::chat::ChatPacket| BotEvent::Chat(p);
        let _ = |p: Arc<ClientboundGamePacket>| BotEvent::Packet(p);
    }
}
