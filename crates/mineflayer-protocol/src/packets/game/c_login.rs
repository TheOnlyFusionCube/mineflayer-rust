use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

use crate::packets::common::CommonPlayerSpawnInfo;

/// The first packet sent by the server to the client after login.
///
/// This packet contains information about the state of the player, the
/// world, and the registry.
#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundLogin {
    pub player_id: MinecraftEntityId,
    pub hardcore: bool,
    pub levels: Vec<Identifier>,
    #[var]
    pub max_players: i32,
    #[var]
    pub chunk_radius: u32,
    #[var]
    pub simulation_distance: u32,
    pub reduced_debug_info: bool,
    pub show_death_screen: bool,
    pub do_limited_crafting: bool,
    pub common: CommonPlayerSpawnInfo,
    pub enforces_secure_chat: bool,
}
