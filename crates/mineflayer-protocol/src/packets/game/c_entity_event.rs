use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundEntityEvent {
    pub entity_id: MinecraftEntityId,
    pub event_id: u8,
}
