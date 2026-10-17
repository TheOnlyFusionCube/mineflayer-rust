use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use uuid::Uuid;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundResourcePackPop {
    pub id: Option<Uuid>,
}
