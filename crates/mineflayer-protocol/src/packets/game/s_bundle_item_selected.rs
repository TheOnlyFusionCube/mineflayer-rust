use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundBundleItemSelected {
    #[var]
    pub slot_id: i32,
    #[var]
    pub selected_item_index: u32,
}
