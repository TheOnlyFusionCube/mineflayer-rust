use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use mineflayer_buf::McBuf;
use mineflayer_protocol::packets::config::s_custom_payload::ServerboundCustomPayload;

use super::packet::config::SendConfigPacketEvent;
use crate::{client_information::send_client_information, packet::login::InLoginState};

/// Send a [`ServerboundCustomPayload`] with "vanilla" as the brand on join.
///
/// You can disable this plugin (`ClientBuilder::new_without_plugins`)
/// and register your own system if you'd like to send a different brand.
pub struct BrandPlugin;
impl Plugin for BrandPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, send_brand.before(send_client_information));
    }
}

pub fn send_brand(mut commands: Commands, mut removed: RemovedComponents<InLoginState>) {
    for entity in removed.read() {
        let mut brand_data = Vec::new();
        // pretend to be vanilla
        "vanilla".to_owned().mc_write(&mut brand_data).unwrap();
        commands.trigger(SendConfigPacketEvent::new(
            entity,
            ServerboundCustomPayload {
                identifier: "brand".into(),
                data: brand_data.into(),
            },
        ));
    }
}
