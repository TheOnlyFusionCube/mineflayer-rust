use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_entity::particle::Particle;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::SoundEvent;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundExplode {
    pub center: Vec3,
    pub radius: f32,
    pub block_count: i32,
    pub player_knockback: Option<Vec3>,
    pub explosion_particle: Particle,
    pub explosion_sound: SoundEvent,
    pub block_particles: Vec<Weighted<ExplosionParticleInfo>>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct Weighted<T: McBuf> {
    pub value: T,
    #[var]
    pub weight: i32,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct ExplosionParticleInfo {
    pub particle: Particle,
    pub scaling: f32,
    pub speed: f32,
}
