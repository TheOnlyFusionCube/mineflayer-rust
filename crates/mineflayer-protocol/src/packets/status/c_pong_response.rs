use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundStatusPacket;

#[derive(McBuf, ClientboundStatusPacket, Clone, Debug, PartialEq)]
pub struct ClientboundPongResponse {
    pub time: u64,
}
