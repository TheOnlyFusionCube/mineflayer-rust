use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetExperience {
    pub experience_progress: f32,
    #[var]
    pub experience_level: u32,
    #[var]
    pub total_experience: u32,
}
