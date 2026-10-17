use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetActionBarText {
    pub text: FormattedText,
}
