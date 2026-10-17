use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::MenuKind;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundOpenScreen {
    #[var]
    pub container_id: i32,
    pub menu_type: MenuKind,
    pub title: FormattedText,
}
