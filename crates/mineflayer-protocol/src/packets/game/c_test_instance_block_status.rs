use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_core::position::Vec3;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundTestInstanceBlockStatus {
    pub status: FormattedText,
    pub size: Option<Vec3>,
}
