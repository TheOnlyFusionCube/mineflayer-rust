use mineflayer_buf::McBuf;
use mineflayer_core::position::ChunkPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::debug_subscription::DebugSubscriptionUpdate;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDebugChunkValue {
    pub chunk_pos: ChunkPos,
    pub update: DebugSubscriptionUpdate,
}
