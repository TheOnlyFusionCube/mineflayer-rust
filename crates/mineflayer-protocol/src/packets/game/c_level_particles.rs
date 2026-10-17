use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_entity::particle::Particle;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundLevelParticles {
    pub override_limiter: bool,
    pub always_show: bool,
    pub pos: Vec3,
    pub x_dist: f32,
    pub y_dist: f32,
    pub z_dist: f32,
    pub max_speed: f32,
    pub count: u32,
    pub particle: Particle,
}
