use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_protocol_macros::ClientboundConfigPacket;
use uuid::Uuid;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundResourcePackPush {
    pub id: Uuid,
    pub url: String,
    pub hash: String,
    pub required: bool,
    pub prompt: Option<FormattedText>,
}
