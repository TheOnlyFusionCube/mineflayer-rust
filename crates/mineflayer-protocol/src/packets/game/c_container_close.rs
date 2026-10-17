use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundContainerClose {
    #[var]
    pub container_id: i32,
}
