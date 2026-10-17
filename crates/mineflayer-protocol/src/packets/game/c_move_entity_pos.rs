use mineflayer_buf::McBuf;
use mineflayer_core::delta::PositionDelta8;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMoveEntityPos {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub delta: PositionDelta8,
    pub on_ground: bool,
}
