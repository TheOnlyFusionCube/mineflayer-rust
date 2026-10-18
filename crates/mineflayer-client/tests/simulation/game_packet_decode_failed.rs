use bevy_ecs::message::Messages;
use mineflayer_client::{packet::game::GamePacketDecodeFailedEvent, test_utils::prelude::*};
use mineflayer_protocol::packets::ConnectionProtocol;

#[test]
fn an_undecodable_game_packet_is_reported() {
    let _lock = init_allowing_errors(); // the engine logs the drop

    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    simulation.receive_packet(default_login_packet());
    simulation.tick();

    // the VarInt 16383, an id no game packet has
    simulation.receive_raw_packet(vec![0xff, 0x7f]);
    simulation.tick();

    let failures: Vec<_> = simulation
        .app
        .world_mut()
        .resource_mut::<Messages<GamePacketDecodeFailedEvent>>()
        .drain()
        .collect();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].entity, simulation.entity);
    assert_eq!(failures[0].packet_id, Some(16383));
    assert!(failures[0].error.contains("16383"), "{}", failures[0].error);
}
