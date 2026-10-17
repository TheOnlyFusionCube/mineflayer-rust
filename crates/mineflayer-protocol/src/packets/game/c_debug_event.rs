use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::debug_subscription::DebugSubscriptionEvent;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDebugEvent {
    pub event: DebugSubscriptionEvent,
}
