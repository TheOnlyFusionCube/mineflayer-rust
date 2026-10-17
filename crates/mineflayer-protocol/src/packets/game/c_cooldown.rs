use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::ItemKind;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundCooldown {
    pub item: ItemKind,
    #[var]
    pub duration: u32,
}
