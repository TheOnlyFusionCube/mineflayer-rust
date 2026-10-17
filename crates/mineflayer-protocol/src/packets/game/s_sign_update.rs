use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSignUpdate {
    pub pos: BlockPos,
    pub is_front_text: bool,
    pub lines: [String; 4],
}
