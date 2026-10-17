use mineflayer_buf::McBuf;
use mineflayer_core::entity_id::MinecraftEntityId;
use mineflayer_inventory::components::AttributeModifier;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::Attribute;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateAttributes {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub values: Vec<AttributeSnapshot>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct AttributeSnapshot {
    pub attribute: Attribute,
    pub base: f64,
    pub modifiers: Vec<AttributeModifier>,
}
