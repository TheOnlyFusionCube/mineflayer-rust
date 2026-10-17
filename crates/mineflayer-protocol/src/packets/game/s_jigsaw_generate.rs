use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundJigsawGenerate {
    pub pos: BlockPos,
    #[var]
    pub levels: u32,
    pub keep_jigsaws: bool,
}
