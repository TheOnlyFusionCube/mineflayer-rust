use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::packets::common::CommonPlayerSpawnInfo;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundRespawn {
    pub common: CommonPlayerSpawnInfo,
    pub data_to_keep: u8,
}
