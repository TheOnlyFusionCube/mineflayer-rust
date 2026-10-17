use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundStatusPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundStatusPacket)]
pub struct ServerboundPingRequest {
    pub time: u64,
}
