use mineflayer_buf::McBuf;
use mineflayer_core::position::GlobalPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetDefaultSpawnPosition {
    pub global_pos: GlobalPos,
    pub yaw: f32,
    pub pitch: f32,
}
