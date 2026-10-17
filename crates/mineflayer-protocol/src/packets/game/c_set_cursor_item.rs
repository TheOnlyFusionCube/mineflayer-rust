use mineflayer_buf::McBuf;
use mineflayer_inventory::ItemStack;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetCursorItem {
    pub contents: ItemStack,
}
