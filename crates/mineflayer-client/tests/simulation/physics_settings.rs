use mineflayer_client::test_utils::prelude::*;
use mineflayer_core::position::ChunkPos;
use mineflayer_entity::Position;
use mineflayer_physics::PhysicsSettings;
use mineflayer_protocol::packets::ConnectionProtocol;

/// How far the bot falls in 10 ticks with `settings`.
fn fall(settings: PhysicsSettings) -> f64 {
    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    simulation.with_resource_mut::<PhysicsSettings>(|mut s| *s = settings);
    simulation.receive_packet(default_login_packet());
    simulation.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
    simulation.tick();
    simulation.tick();
    let start = simulation.component::<Position>().y;
    for _ in 0..10 {
        simulation.tick();
    }
    start - simulation.component::<Position>().y
}

#[test]
fn gravity_is_a_setting() {
    let _lock = init();
    let vanilla = fall(PhysicsSettings::default());
    let half = fall(PhysicsSettings {
        gravity: 0.04,
        ..PhysicsSettings::default()
    });
    assert!(vanilla > 0., "the bot falls");
    assert!(
        (half - vanilla / 2.).abs() < vanilla * 0.05,
        "half the gravity, half the fall: {half} of {vanilla}"
    );
}
