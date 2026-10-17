use mineflayer_buf::McBuf;
use mineflayer_core::entity_id::MinecraftEntityId;
use mineflayer_entity::EntityMetadataItems;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetEntityData {
    #[var]
    pub id: MinecraftEntityId,
    pub packed_items: EntityMetadataItems,
}
