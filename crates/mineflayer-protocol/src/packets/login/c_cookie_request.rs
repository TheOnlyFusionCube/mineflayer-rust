use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundLoginPacket;

#[derive(McBuf, ClientboundLoginPacket, Clone, Debug, PartialEq)]
pub struct ClientboundCookieRequest {
    pub key: Identifier,
}
