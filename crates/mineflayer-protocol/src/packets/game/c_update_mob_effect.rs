use mineflayer_buf::McBuf;
use mineflayer_entity::MobEffectData;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::MobEffect;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateMobEffect {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub mob_effect: MobEffect,
    pub data: MobEffectData,
}
