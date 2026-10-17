use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundConfigPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundKeepAlive {
    pub id: u64,
}
