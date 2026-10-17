use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundLoginPacket;

#[derive(McBuf, ClientboundLoginPacket, Clone, Debug, PartialEq)]
pub struct ClientboundHello {
    #[limit(20)]
    pub server_id: String,
    pub public_key: Vec<u8>,
    pub challenge: Vec<u8>,
    pub should_authenticate: bool,
}
