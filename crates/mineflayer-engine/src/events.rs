//! Defines the [`enum@Event`] enum and makes those events trigger when they're
//! sent in the ECS.

use std::{sync::Arc, time::Duration};

use bevy_app::{App, Plugin, PreUpdate, Update};
use bevy_ecs::prelude::*;
use derive_more::{Deref, DerefMut};
use mineflayer_chat::FormattedText;
use mineflayer_client::join::ConnectionFailedEvent;
use mineflayer_core::{
    position::{BlockPos, ChunkPos},
    tick::GameTick,
};
use mineflayer_entity::{Dead, InLoadedChunk};
use mineflayer_protocol::{
    connect::ConnectionError, packets::game::c_player_combat_kill::ClientboundPlayerCombatKill,
};
use mineflayer_world::WorldName;
use tokio::sync::mpsc;

use crate::{
    chunks::ReceiveChunkEvent,
    client_chat::{ChatPacket, ChatReceivedEvent},
    disconnect::DisconnectEvent,
    packet::game::{
        AddPlayerEvent, DeathEvent, GamePacketDecodeFailedEvent, KeepAliveEvent, RemovePlayerEvent,
        UpdatePlayerEvent,
    },
    pathfinder::GotoId,
    player::PlayerInfo,
};

// (for contributors):
// HOW TO ADD A NEW (packet based) EVENT:
// - Add it as an ECS event first:
//     - Make a struct that contains an entity field and some data fields (look
//       in packet/game/events.rs for examples. These structs should always have
//       their names end with "Event".
//         - (the `entity` field is the local player entity that's receiving the
//           event)
//     - In the GamePacketHandler, you always have a `player` field that you can
//       use.
//     - Add the event struct in PacketPlugin::build
//         - (in the `impl Plugin for PacketPlugin`)
//     - To get the event writer, you have to get an MessageWriter<ThingEvent>.
//       Look at other packets in packet/game/mod.rs for examples.
//
// At this point, you've created a new ECS event. That's annoying for bots to
// use though, so you might wanna add it to the Event enum too:
//     - In this file, add a new variant to that Event enum with the same name
//       as your event (without the "Event" suffix).
//     - Create a new system function like the other ones here, and put that
//       system function in the `impl Plugin for EventsPlugin`

/// Something that happened in-game, such as a tick passing or chat message
/// being sent.
///
/// Note: Events are sent before they're processed, so for example game ticks
/// happen at the beginning of a tick before anything has happened.
#[derive(Clone, Debug)]
pub enum Event {
    /// Happens right after the bot switches into the Game state, but before
    /// it's actually spawned.
    ///
    /// This can be useful for setting the client information with
    /// [`Client::set_client_information`], so the packet doesn't have to be
    /// sent twice.
    ///
    /// You may want to use [`Event::Spawn`] instead to wait for the bot to be
    /// in the world.
    ///
    /// [`Client::set_client_information`]: crate::Client::set_client_information
    Init,
    /// Fired when we receive a login packet, which is after [`Event::Init`] but
    /// before [`Event::Spawn`]. You usually want [`Event::Spawn`] instead.
    ///
    /// Your position may be [`Vec3::ZERO`] immediately after you receive this
    /// event, but it'll be ready by the time you get [`Event::Spawn`].
    ///
    /// It's possible for this event to be sent multiple times per client if a
    /// server sends multiple login packets (like when switching worlds).
    ///
    /// [`Vec3::ZERO`]: mineflayer_core::position::Vec3::ZERO
    Login,
    /// Fired when the player fully spawns into the world (is in a loaded chunk)
    /// and is ready to interact with it.
    ///
    /// This is usually the event you should listen for when waiting for the bot
    /// to be ready.
    ///
    /// This event will be sent every time the client respawns or switches
    /// worlds, as long as the server sends chunks to the client.
    Spawn,
    /// A chat message was sent in the game chat.
    Chat(ChatPacket),
    /// Happens 20 times per second, but only when the world is loaded.
    Tick,
    #[cfg(feature = "packet-event")]
    /// We received a packet from the server.
    ///
    /// ```
    /// # use mineflayer_engine::Event;
    /// # use mineflayer_protocol::packets::game::ClientboundGamePacket;
    /// # async fn example(event: Event) {
    /// # match event {
    /// Event::Packet(packet) => match &*packet {
    ///     ClientboundGamePacket::Login(_) => {
    ///         println!("login packet");
    ///     }
    ///     _ => {}
    /// },
    /// # _ => {}
    /// # }
    /// # }
    /// ```
    Packet(Arc<mineflayer_protocol::packets::game::ClientboundGamePacket>),
    /// A player joined the game (or more specifically, was added to the tab
    /// list).
    AddPlayer(PlayerInfo),
    /// A player left the game (or maybe is still in the game and was just
    /// removed from the tab list).
    RemovePlayer(PlayerInfo),
    /// A player was updated in the tab list (gamemode, display
    /// name, or latency changed).
    UpdatePlayer(PlayerInfo),
    /// The client player died in-game.
    Death(Option<Arc<ClientboundPlayerCombatKill>>),
    /// A `KeepAlive` packet was sent by the server.
    KeepAlive(u64),
    /// The client disconnected from the server.
    ///
    /// Also see [`Event::ConnectionFailed`].
    Disconnect(Option<FormattedText>),
    /// The initial connection to the server failed.
    ///
    /// Also see [`Event::Disconnect`], and the related ECS event
    /// [`ConnectionFailedEvent`].
    ConnectionFailed(Arc<ConnectionError>),
    ReceiveChunk(ChunkPos),
    /// A game packet arrived but did not decode, so the engine dropped it.
    PacketDecodeFailed {
        /// The packet id, if the decoder read that far.
        packet_id: Option<u32>,
        /// The decode error.
        error: String,
    },
    /// A pathfinder result — see [`PathEvent`].
    Pathfinder(PathEvent),
    /// The server offers a resource pack, in the configuration or the
    /// game phase.
    ResourcePack(ResourcePackOffer),
}

/// A resource pack that the server offers. The reply goes out in the
/// phase the client is in when it answers.
#[derive(Clone, Debug)]
pub struct ResourcePackOffer {
    /// The pack's id; a reply must name it.
    pub id: uuid::Uuid,
    pub url: String,
    pub hash: String,
    /// The server disconnects a client that declines it.
    pub required: bool,
    /// The offer came in the configuration phase.
    pub in_config: bool,
}

/// A pathfinder result, lifted from the ECS messages into the client
/// event stream (the port used to infer these by diffing components
/// every tick).
#[derive(Clone, Debug)]
pub enum PathEvent {
    /// A search finished. A non-empty `path` with `is_partial == true`
    /// leads toward the goal but not to it. An empty `path` with
    /// `is_partial == true` means that no route exists or that the search
    /// used its whole budget.
    PathFound {
        /// The goto the search belongs to.
        goto: GotoId,
        /// The path nodes the bot will walk, from its own position.
        path: Vec<BlockPos>,
        /// Whether the search gave up before reaching the goal.
        is_partial: bool,
        /// How long the A* search ran.
        elapsed: Duration,
    },
    /// The partial searches of a goto used its
    /// [`PathfinderOpts::total_timeout`](crate::pathfinder::PathfinderOpts::total_timeout).
    /// Sent once per run of partial searches; the goal stays, and the bot
    /// keeps walking the partial path.
    Timeout {
        /// The goto that used its time.
        goto: GotoId,
    },
    /// The goal's success check holds — the path executed to its end, or
    /// the goal was already satisfied.
    GoalReached {
        /// The goto that reached its goal.
        goto: GotoId,
    },
    /// The goal was abandoned without reaching it (a stop, or a world
    /// change).
    Stopped {
        /// The goto that was stopped.
        goto: GotoId,
    },
}

/// A component that contains an event sender for events that are only
/// received by local players.
///
/// The receiver for this is returned by
/// [`Client::start_client`](crate::Client::start_client).
#[derive(Component, Deref, DerefMut)]
pub struct LocalPlayerEvents(pub mpsc::UnboundedSender<Event>);

pub struct EventsPlugin;
impl Plugin for EventsPlugin {
    fn build(&self, app: &mut App) {
        // chained: listeners of one pass with no order would send their
        // events in an order that changes from run to run. The raw packets
        // go first, then what derives from them, the way a protocol client
        // emits `packet` before the named packet event.
        app.add_systems(
            Update,
            (
                #[cfg(feature = "packet-event")]
                packet_listener,
                packet_decode_failed_listener,
                login_listener,
                receive_chunk_listener,
                chat_listener,
                resource_pack_listener,
                add_player_listener,
                update_player_listener,
                remove_player_listener,
                spawn_listener,
                death_listener.after(mineflayer_client::packet::death_event_on_0_health),
                disconnect_listener,
                connection_failed_listener
                    .after(mineflayer_client::join::poll_create_connection_task),
            )
                .chain(),
        )
        .add_systems(
            PreUpdate,
            init_listener.before(super::connection::read_packets),
        )
        .add_systems(GameTick, tick_listener)
        .add_observer(keepalive_listener);
    }
}

// when LocalPlayerEvents is added, it means the client just started
pub fn init_listener(query: Query<&LocalPlayerEvents, Added<LocalPlayerEvents>>) {
    for local_player_events in &query {
        let _ = local_player_events.send(Event::Init);
    }
}

/// One [`Event::Login`] for each login packet, as upstream mineflayer
/// emits `login`; a proxy that moves the bot to another server sends a
/// second one.
pub fn login_listener(
    query: Query<&LocalPlayerEvents>,
    mut packets: MessageReader<super::packet::game::ReceiveGamePacketEvent>,
    mut commands: Commands,
) {
    for event in packets.read() {
        if !matches!(
            *event.packet,
            mineflayer_protocol::packets::game::ClientboundGamePacket::Login(_)
        ) {
            continue;
        }
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::Login);
        }
        commands.entity(event.entity).remove::<SentSpawnEvent>();
    }
}

/// A unit struct component that indicates that the entity has sent
/// [`Event::Spawn`].
///
/// This is just used internally by the [`spawn_listener`] system to avoid
/// sending the event twice if we stop being in an unloaded chunk. It's removed
/// when we receive a login packet.
#[derive(Component)]
pub struct SentSpawnEvent;
#[allow(clippy::type_complexity)]
pub fn spawn_listener(
    query: Query<(Entity, &LocalPlayerEvents), (Added<InLoadedChunk>, Without<SentSpawnEvent>)>,
    mut commands: Commands,
) {
    for (entity, local_player_events) in &query {
        let _ = local_player_events.send(Event::Spawn);
        commands.entity(entity).insert(SentSpawnEvent);
    }
}

pub fn chat_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<ChatReceivedEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::Chat(event.packet.clone()));
        }
    }
}

pub fn resource_pack_listener(
    query: Query<(&LocalPlayerEvents, Has<mineflayer_client::InConfigState>)>,
    mut events: MessageReader<mineflayer_client::packet::game::ResourcePackEvent>,
) {
    for event in events.read() {
        if let Ok((local_player_events, in_config)) = query.get(event.entity) {
            let _ = local_player_events.send(Event::ResourcePack(ResourcePackOffer {
                id: event.id,
                url: event.url.clone(),
                hash: event.hash.clone(),
                required: event.required,
                in_config,
            }));
        }
    }
}

// only tick if we're in a world
pub fn tick_listener(query: Query<&LocalPlayerEvents, With<WorldName>>) {
    for local_player_events in &query {
        let _ = local_player_events.send(Event::Tick);
    }
}

#[cfg(feature = "packet-event")]
pub fn packet_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<super::packet::game::ReceiveGamePacketEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::Packet(event.packet.clone()));
        }
    }
}

pub fn packet_decode_failed_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<GamePacketDecodeFailedEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::PacketDecodeFailed {
                packet_id: event.packet_id,
                error: event.error.clone(),
            });
        }
    }
}

pub fn add_player_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<AddPlayerEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::AddPlayer(event.info.clone()));
        }
    }
}

pub fn update_player_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<UpdatePlayerEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::UpdatePlayer(event.info.clone()));
        }
    }
}

pub fn remove_player_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<RemovePlayerEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::RemovePlayer(event.info.clone()));
        }
    }
}

pub fn death_listener(query: Query<&LocalPlayerEvents>, mut events: MessageReader<DeathEvent>) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::Death(event.packet.clone().map(|p| p.into())));
        }
    }
}

/// Send the "Death" event for [`LocalEntity`]s that died with no reason.
///
/// [`LocalEntity`]: mineflayer_entity::LocalEntity
pub fn dead_component_listener(query: Query<&LocalPlayerEvents, Added<Dead>>) {
    for local_player_events in &query {
        local_player_events.send(Event::Death(None)).unwrap();
    }
}

pub fn keepalive_listener(keep_alive: On<KeepAliveEvent>, query: Query<&LocalPlayerEvents>) {
    if let Ok(local_player_events) = query.get(keep_alive.entity) {
        let _ = local_player_events.send(Event::KeepAlive(keep_alive.id));
    }
}

pub fn disconnect_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<DisconnectEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::Disconnect(event.reason.clone()));
        }
    }
}

pub fn connection_failed_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<ConnectionFailedEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::ConnectionFailed(event.error.clone()));
        }
    }
}

pub fn receive_chunk_listener(
    query: Query<&LocalPlayerEvents>,
    mut events: MessageReader<ReceiveChunkEvent>,
) {
    for event in events.read() {
        if let Ok(local_player_events) = query.get(event.entity) {
            let _ = local_player_events.send(Event::ReceiveChunk(ChunkPos::new(
                event.packet.x,
                event.packet.z,
            )));
        }
    }
}
