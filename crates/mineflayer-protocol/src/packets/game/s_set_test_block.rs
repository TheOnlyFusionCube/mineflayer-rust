use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetTestBlock {
    pub position: BlockPos,
    pub mode: TestBlockMode,
    pub message: String,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum TestBlockMode {
    #[default]
    Start,
    Log,
    Fail,
    Accept,
}
