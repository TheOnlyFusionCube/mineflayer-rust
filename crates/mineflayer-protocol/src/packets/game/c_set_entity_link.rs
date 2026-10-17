use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetEntityLink {
    pub source_id: MinecraftEntityId,
    pub dest_id: MinecraftEntityId,
}
