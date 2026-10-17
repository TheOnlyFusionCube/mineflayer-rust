use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundBlockEntityTagQuery {
    #[var]
    pub transaction_id: u32,
    pub pos: BlockPos,
}
