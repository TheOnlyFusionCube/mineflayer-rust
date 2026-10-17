use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundTakeItemEntity {
    #[var]
    pub item_id: u32,
    #[var]
    pub player_id: MinecraftEntityId,
    #[var]
    pub amount: u32,
}
