use mineflayer_buf::{McBuf, UnsizedByteArray};
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundCustomPayload {
    pub identifier: Identifier,
    pub data: UnsizedByteArray,
}
