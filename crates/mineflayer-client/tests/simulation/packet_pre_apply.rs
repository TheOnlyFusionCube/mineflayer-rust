use bevy_ecs::prelude::*;
use mineflayer_client::{connection::PacketPreApply, test_utils::prelude::*};
use mineflayer_core::{entity_id::MinecraftEntityId, position::ChunkPos};
use mineflayer_protocol::packets::{
    ConnectionProtocol,
    game::{ClientboundGamePacket, ClientboundRemoveEntities},
};
use mineflayer_registry::builtin::EntityKind;

#[derive(Default, Resource)]
struct SeenBeforeRemove(Vec<bool>);

fn record(world: &mut World, _client: Entity, packet: &ClientboundGamePacket) {
    let ClientboundGamePacket::RemoveEntities(p) = packet else {
        return;
    };
    let ids: Vec<MinecraftEntityId> = world
        .query::<&MinecraftEntityId>()
        .iter(world)
        .copied()
        .collect();
    let seen = p.entity_ids.iter().map(|id| ids.contains(id)).collect();
    world.resource_mut::<SeenBeforeRemove>().0 = seen;
}

#[test]
fn a_hook_sees_an_entity_before_its_removal() {
    let _lock = init();

    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    simulation.app.init_resource::<SeenBeforeRemove>();
    simulation
        .app
        .world_mut()
        .resource_mut::<PacketPreApply>()
        .0
        .push(record);
    simulation.receive_packet(default_login_packet());
    simulation.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
    simulation.tick();
    simulation.receive_packet(make_basic_add_entity(EntityKind::Cow, 123, (0.5, 64., 0.5)));
    simulation.tick();

    simulation.receive_packet(ClientboundRemoveEntities {
        entity_ids: vec![MinecraftEntityId(123)],
    });
    simulation.tick();

    assert_eq!(
        simulation.app.world().resource::<SeenBeforeRemove>().0,
        [true],
        "the hook ran before the despawn"
    );
    let still_there = simulation
        .app
        .world_mut()
        .query::<&MinecraftEntityId>()
        .iter(simulation.app.world())
        .any(|id| *id == MinecraftEntityId(123));
    assert!(!still_there, "the engine then despawned it");
}
