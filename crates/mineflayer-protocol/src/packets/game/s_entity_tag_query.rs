use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundEntityTagQuery {
    #[var]
    pub transaction_id: u32,
    #[var]
    pub entity_id: MinecraftEntityId,
}
