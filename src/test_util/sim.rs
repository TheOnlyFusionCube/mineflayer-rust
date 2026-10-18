//! `BotSim`: a [`Bot`] over the engine's real systems and the real bridge,
//! fed clientbound packets with no network and no server.
//!
//! It builds the app from the bot's plugins like `connect` does, takes the
//! world out the way the engine's runner does, and runs the schedules by
//! hand: one [`BotSim::tick`] is one `Main` pass and one `GameTick`, the
//! runner's step. The engine events of the step then go through
//! [`Bridge::handle`], so a test sees the `BotEvent`s a connected bot
//! would emit, in the same order.

use std::sync::Arc;

use mineflayer_engine::{
    app::Main,
    connection::RawConnection,
    core::{position::ChunkPos, tick::GameTick},
    ecs::world::World,
    events::{Event, LocalPlayerEvents},
    protocol::packets::{
        ConnectionProtocol, Packet,
        game::{ClientboundGamePacket, ServerboundGamePacket},
    },
    test_utils::simulation::{
        SentPackets, Simulation, default_login_packet, make_basic_empty_chunk,
    },
};
use parking_lot::RwLock;
use tokio::sync::{broadcast, mpsc, watch};

use crate::{Bot, BotBuilder, BotEvent, bridge::Bridge};

pub struct BotSim {
    /// The bot under test.
    pub bot: Bot,
    ecs: Arc<RwLock<World>>,
    engine_events: mpsc::UnboundedReceiver<Event>,
    bridge: Bridge,
    events: broadcast::Receiver<BotEvent>,
    sent: SentPackets,
    // the bridge's liveness channel; kept so the bot sees a live
    // connection
    _alive: watch::Receiver<()>,
    // the engine's tasks need their runtime alive
    _rt: tokio::runtime::Runtime,
}

impl Default for BotSim {
    fn default() -> Self {
        Self::new()
    }
}

impl BotSim {
    /// A bot that has logged in and spawned in an empty chunk at the
    /// origin.
    pub fn new() -> Self {
        Self::spawned(BotBuilder::default())
    }

    /// [`Self::new`] for a bot built from `builder`.
    pub fn spawned(builder: BotBuilder) -> Self {
        let mut sim = Self::connected(builder);
        // in separate passes, as a server's packets arrive
        sim.receive(default_login_packet());
        sim.tick();
        sim.receive(make_basic_empty_chunk(ChunkPos::new(0, 0), 384 / 16));
        sim.tick();
        // the engine marks the bot as in a loaded chunk one pass later
        sim.tick();
        sim
    }

    /// A bot in the game state that has received no game packet yet.
    pub fn connected(builder: BotBuilder) -> Self {
        let mut sim =
            Simulation::new_with_plugins(ConnectionProtocol::Game, builder.plugins.clone());
        let sent = SentPackets::new(&mut sim);
        let (tx, engine_events) = mpsc::unbounded_channel();
        sim.app
            .world_mut()
            .entity_mut(sim.entity)
            .insert(LocalPlayerEvents(tx));
        let Simulation {
            mut app,
            entity,
            rt,
            ..
        } = sim;
        let ecs = Arc::new(RwLock::new(std::mem::take(app.world_mut())));
        let client = mineflayer_engine::Client::new(entity, ecs.clone());

        let (bot, bridge, alive) = builder.attach(client);
        let events = bot.events();
        Self {
            bot,
            ecs,
            engine_events,
            bridge,
            events,
            sent,
            _alive: alive,
            _rt: rt,
        }
    }

    /// Queue a clientbound packet; the next [`Self::tick`] reads it.
    pub fn receive(&mut self, packet: impl Packet<ClientboundGamePacket>) {
        let bytes = mineflayer_engine::protocol::write::serialize_packet(&packet.into_variant())
            .expect("a test packet serializes");
        let entity = self.bot.client.entity;
        self.ecs
            .write()
            .entity_mut(entity)
            .get_mut::<RawConnection>()
            .expect("the bot has a connection")
            .injected_clientbound_packets
            .push(bytes);
    }

    /// One step of the engine's runner, then the bridge on the engine
    /// events of that step.
    pub fn tick(&mut self) {
        {
            let mut ecs = self.ecs.write();
            ecs.run_schedule(Main);
            ecs.run_schedule(GameTick);
        }
        while let Ok(event) = self.engine_events.try_recv() {
            self.bridge.handle(event);
        }
    }

    /// The `BotEvent`s since the last call.
    pub fn events(&mut self) -> Vec<BotEvent> {
        let mut out = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => out.push(event),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    panic!("BotSim lost {n} events; drain them more often")
                }
                Err(_) => return out,
            }
        }
    }

    /// The serverbound packets since the last call.
    pub fn sent(&mut self) -> Vec<ServerboundGamePacket> {
        std::iter::from_fn(|| self.sent.next()).collect()
    }
}

/// The variant name of each event, for order checks.
pub fn names(events: &[BotEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            let debug = format!("{e:?}");
            debug
                .split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{
        BlockPos,
        block::BlockState,
        protocol::packets::game::{ClientboundBlockUpdate, ClientboundSystemChat},
        registry::builtin::BlockKind,
    };

    use super::*;

    /// `want` appears in `got` in order, with other events between.
    fn assert_in_order(got: &[String], want: &[&str]) {
        let mut rest = got.iter();
        for name in want {
            assert!(
                rest.any(|g| g == name),
                "{name} missing or out of order in {got:?}"
            );
        }
    }

    #[test]
    fn login_gives_connected_login_spawn() {
        let mut sim = BotSim::new();
        let names = names(&sim.events());
        assert_in_order(&names, &["Init", "Connected", "Login", "Spawn"]);
    }

    /// Upstream emits `login` for each login packet, also for the second
    /// one of a proxy that moves the bot to another server.
    #[test]
    fn each_login_packet_gives_login() {
        let mut sim = BotSim::new();
        sim.events();
        sim.receive(default_login_packet());
        sim.tick();
        let names = names(&sim.events());
        assert_eq!(
            names.iter().filter(|n| *n == "Login").count(),
            1,
            "{names:?}"
        );
    }

    #[test]
    fn a_chat_packet_gives_chat_message_message_str() {
        let mut sim = BotSim::new();
        sim.events();
        sim.receive(ClientboundSystemChat {
            content: "hello".into(),
            overlay: false,
        });
        sim.tick();
        let events = sim.events();
        assert_in_order(&names(&events), &["Chat", "Message", "MessageStr"]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, BotEvent::MessageStr { message, .. } if message == "hello")),
            "{events:?}"
        );
    }

    #[test]
    fn a_block_update_gives_block_update() {
        let mut sim = BotSim::new();
        sim.events();
        let pos = BlockPos::new(1, 2, 3);
        sim.receive(ClientboundBlockUpdate {
            pos,
            block_state: BlockState::from(BlockKind::Stone),
        });
        sim.tick();
        let events = sim.events();
        assert!(
            events.iter().any(|e| matches!(
                e,
                BotEvent::BlockUpdate { new, .. } if new.pos == pos
                    && new.kind() == BlockKind::Stone
            )),
            "{:?}",
            names(&events)
        );
        assert_eq!(
            sim.bot.block_at(pos).map(|b| b.kind()),
            Some(BlockKind::Stone)
        );
        let _ = sim.sent();
    }

    /// A keybind in a container title used to fail the decode of
    /// `open_screen`, so the window never opened.
    #[test]
    fn a_container_with_a_keybind_title_opens() {
        use mineflayer_engine::{
            FormattedText,
            inventory::ItemStack,
            protocol::packets::game::{ClientboundContainerSetContent, ClientboundOpenScreen},
            registry::builtin::MenuKind,
        };
        use serde::Deserialize;

        let mut sim = BotSim::new();
        sim.events();
        let title =
            FormattedText::deserialize(serde_json::json!({"keybind": "key.inventory"})).unwrap();
        sim.receive(ClientboundOpenScreen {
            container_id: 3,
            menu_type: MenuKind::Generic9x3,
            title,
        });
        sim.receive(ClientboundContainerSetContent {
            container_id: 3,
            state_id: 1,
            items: vec![ItemStack::Empty; 27 + 36],
            carried_item: ItemStack::Empty,
        });
        sim.tick();
        let events = sim.events();
        let window = events
            .iter()
            .find_map(|e| match e {
                BotEvent::WindowOpen(window) => Some(window),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no WindowOpen in {:?}", names(&events)));
        assert_eq!(window.id(), 3);
    }
}
