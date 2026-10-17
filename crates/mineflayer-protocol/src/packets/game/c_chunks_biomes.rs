use mineflayer_buf::McBuf;
use mineflayer_core::position::ChunkPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundChunksBiomes {
    pub chunk_biome_data: Vec<ChunkBiomeData>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ChunkBiomeData {
    pub pos: ChunkPos,
    pub buffer: Vec<u8>,
}
