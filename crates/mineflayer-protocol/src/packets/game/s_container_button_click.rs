use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundContainerButtonClick {
    #[var]
    pub container_id: i32,
    #[var]
    pub button_id: u32,
}
