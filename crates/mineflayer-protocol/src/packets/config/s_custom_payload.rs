use mineflayer_buf::{McBuf, UnsizedByteArray};
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundConfigPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundCustomPayload {
    pub identifier: Identifier,
    pub data: UnsizedByteArray,
}
