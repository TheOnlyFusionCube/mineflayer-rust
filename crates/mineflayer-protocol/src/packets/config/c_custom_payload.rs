use mineflayer_buf::{McBuf, UnsizedByteArray};
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundConfigPacket;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundCustomPayload {
    pub identifier: Identifier,
    pub data: UnsizedByteArray,
}
