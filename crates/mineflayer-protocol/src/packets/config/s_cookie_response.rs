use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundConfigPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundCookieResponse {
    pub key: Identifier,
    pub payload: Option<Vec<u8>>,
}
