use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundLoginPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundLoginPacket)]
pub struct ServerboundLoginAcknowledged;
