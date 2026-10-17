use std::sync::Arc;

use mineflayer_buf::McBuf;
use mineflayer_core::bitset::BitSet;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundLightUpdate {
    #[var]
    pub x: i32,
    #[var]
    pub z: i32,
    pub light_data: ClientboundLightUpdatePacketData,
}

#[derive(McBuf, Clone, Debug, Default, PartialEq)]
pub struct ClientboundLightUpdatePacketData {
    pub sky_y_mask: BitSet,
    pub block_y_mask: BitSet,
    pub empty_sky_y_mask: BitSet,
    pub empty_block_y_mask: BitSet,
    pub sky_updates: Arc<Box<[Box<[u8]>]>>,
    pub block_updates: Arc<Box<[Box<[u8]>]>>,
}
