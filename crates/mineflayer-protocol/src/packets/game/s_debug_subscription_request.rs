use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_registry::builtin::DebugSubscription;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundDebugSubscriptionRequest {
    pub subscriptions: Vec<DebugSubscription>,
}
