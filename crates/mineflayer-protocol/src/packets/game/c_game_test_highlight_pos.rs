use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundGameTestHighlightPos {
    pub absolute_pos: BlockPos,
    pub relative_pos: BlockPos,
}
