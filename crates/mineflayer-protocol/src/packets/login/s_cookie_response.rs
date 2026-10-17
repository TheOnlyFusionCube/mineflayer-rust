use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundLoginPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundLoginPacket)]
pub struct ServerboundCookieResponse {
    pub key: Identifier,
    pub payload: Option<Vec<u8>>,
}
