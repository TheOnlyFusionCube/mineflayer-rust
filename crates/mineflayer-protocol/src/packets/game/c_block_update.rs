use mineflayer_block::BlockState;
use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundBlockUpdate {
    pub pos: BlockPos,
    pub block_state: BlockState,
}
