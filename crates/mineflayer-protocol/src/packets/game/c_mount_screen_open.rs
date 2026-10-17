use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMountScreenOpen {
    #[var]
    pub container_id: i32,
    #[var]
    pub inventory_columns: u32,
    pub entity_id: MinecraftEntityId,
}
