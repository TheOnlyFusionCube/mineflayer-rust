use mineflayer_buf::McBuf;
use mineflayer_core::position::ChunkPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundForgetLevelChunk {
    pub pos: ChunkPos,
}
