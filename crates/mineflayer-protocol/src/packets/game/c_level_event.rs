use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundLevelEvent {
    pub event_type: u32,
    pub pos: BlockPos,
    pub data: u32,
    pub global_event: bool,
}
