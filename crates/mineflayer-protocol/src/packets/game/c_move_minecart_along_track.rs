use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMoveMinecartAlongTrack {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub lerp_steps: Vec<MinecartStep>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct MinecartStep {
    pub position: Vec3,
    pub movement: Vec3,
    pub y_rot: u8,
    pub x_rot: u8,
    pub weight: f32,
}
