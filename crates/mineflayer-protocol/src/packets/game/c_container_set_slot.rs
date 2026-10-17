use mineflayer_buf::McBuf;
use mineflayer_inventory::ItemStack;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundContainerSetSlot {
    #[var]
    pub container_id: i32,
    /// An identifier used by the server to track client inventory desyncs.
    #[var]
    pub state_id: u32,
    /// The slot index.
    ///
    /// See <https://minecraft.wiki/w/Java_Edition_protocol/Inventory>.
    pub slot: u16,
    pub item_stack: ItemStack,
}
