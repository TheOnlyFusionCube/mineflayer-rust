use std::sync::Arc;

use mineflayer_buf::McBuf;
use mineflayer_core::heightmap_kind::HeightmapKind;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::BlockEntityKind;
use simdnbt::owned::Nbt;

use super::c_light_update::ClientboundLightUpdatePacketData;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundLevelChunkWithLight {
    // this can't be a ChunkPos since that reads z first and then x
    pub x: i32,
    pub z: i32,
    pub chunk_data: ClientboundLevelChunkPacketData,
    pub light_data: ClientboundLightUpdatePacketData,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ClientboundLevelChunkPacketData {
    pub heightmaps: Vec<(HeightmapKind, Box<[u64]>)>,
    /// The raw chunk sections.
    ///
    /// We can't parse the data in `mineflayer-protocol` because sometimes we want
    /// to skip parsing this.
    ///
    /// This is an Arc because it's often very big and we want it to be cheap to
    /// clone.
    pub data: Arc<Box<[u8]>>,
    pub block_entities: Vec<BlockEntity>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct BlockEntity {
    pub packed_xz: u8,
    pub y: u16,
    pub kind: BlockEntityKind,
    pub data: Nbt,
}
