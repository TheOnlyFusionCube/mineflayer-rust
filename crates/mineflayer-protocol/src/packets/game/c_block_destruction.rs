use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundBlockDestruction {
    /// The ID of the entity breaking the block.
    #[var]
    pub id: MinecraftEntityId,
    pub pos: BlockPos,
    /// 0–9 to set it, any other value to remove it.
    pub progress: u8,
}
