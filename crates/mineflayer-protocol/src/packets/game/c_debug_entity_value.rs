use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

use crate::common::debug_subscription::DebugSubscriptionUpdate;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDebugEntityValue {
    pub entity_id: MinecraftEntityId,
    pub update: DebugSubscriptionUpdate,
}
