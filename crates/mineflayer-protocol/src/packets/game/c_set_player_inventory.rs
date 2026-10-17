use mineflayer_buf::McBuf;
use mineflayer_inventory::ItemStack;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetPlayerInventory {
    #[var]
    pub slot: u32,
    pub contents: ItemStack,
}
