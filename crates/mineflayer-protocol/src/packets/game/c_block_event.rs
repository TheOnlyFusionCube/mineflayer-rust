use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::BlockKind;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundBlockEvent {
    pub pos: BlockPos,
    pub action_id: u8,
    pub action_parameter: u8,
    pub block: BlockKind,
}
