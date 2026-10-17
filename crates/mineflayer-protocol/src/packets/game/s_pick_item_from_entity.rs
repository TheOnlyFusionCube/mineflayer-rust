use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundPickItemFromEntity {
    #[var]
    pub id: MinecraftEntityId,
    pub include_data: bool,
}
