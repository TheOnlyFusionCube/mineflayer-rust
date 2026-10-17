use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundContainerSetData {
    #[var]
    pub container_id: i32,
    pub id: u16,
    pub value: u16,
}
