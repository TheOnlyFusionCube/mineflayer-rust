use mineflayer_buf::McBuf;
use mineflayer_inventory::ItemStack;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetCreativeModeSlot {
    pub slot_num: u16,
    pub item_stack: ItemStack,
}
