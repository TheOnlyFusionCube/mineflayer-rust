//! Clients send a [`ServerboundClientTickEnd`] packet every tick.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::*;
use mineflayer_core::tick::GameTick;
use mineflayer_entity::LocalEntity;
use mineflayer_physics::PhysicsSystems;
use mineflayer_protocol::packets::game::ServerboundClientTickEnd;
use mineflayer_world::WorldName;

use crate::{mining::MiningSystems, packet::game::SendGamePacketEvent};

/// A plugin that makes clients send a [`ServerboundClientTickEnd`] packet every
/// tick.
pub struct TickEndPlugin;
impl Plugin for TickEndPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            GameTick,
            // this has to happen after every other event that might send packets
            game_tick_packet
                .after(PhysicsSystems)
                .after(MiningSystems)
                .after(crate::movement::send_position),
        );
    }
}

pub fn game_tick_packet(
    query: Query<Entity, (With<LocalEntity>, With<WorldName>)>,
    mut commands: Commands,
) {
    for entity in query.iter() {
        commands.trigger(SendGamePacketEvent::new(entity, ServerboundClientTickEnd));
    }
}
