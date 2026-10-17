use mineflayer_buf::McBuf;
use mineflayer_core::sound::CustomSound;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::SoundEvent;
use mineflayer_core::entity_id::MinecraftEntityId;

use super::c_sound::SoundSource;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSoundEntity {
    pub sound: mineflayer_registry::Holder<SoundEvent, CustomSound>,
    pub source: SoundSource,
    #[var]
    pub id: MinecraftEntityId,
    pub volume: f32,
    pub pitch: f32,
    pub seed: u64,
}
