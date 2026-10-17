use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::debug_subscription::DebugSubscriptionUpdate;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDebugBlockValue {
    pub block_pos: BlockPos,
    pub update: DebugSubscriptionUpdate,
}
