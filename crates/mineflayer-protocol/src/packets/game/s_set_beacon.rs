use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetBeacon {
    #[var]
    pub primary: Option<u32>,
    #[var]
    pub secondary: Option<u32>,
}
