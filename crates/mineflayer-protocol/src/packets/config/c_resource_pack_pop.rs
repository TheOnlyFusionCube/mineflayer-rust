use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;
use uuid::Uuid;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundResourcePackPop {
    pub id: Option<Uuid>,
}
