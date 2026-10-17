use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetCommandMinecart {
    #[var]
    pub entity: u32,
    pub command: String,
    pub track_output: bool,
}
