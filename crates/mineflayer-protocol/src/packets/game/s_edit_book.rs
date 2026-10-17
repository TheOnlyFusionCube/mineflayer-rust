use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundEditBook {
    #[var]
    pub slot: u32,
    pub pages: Vec<String>,
    pub title: Option<String>,
}
