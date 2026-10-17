use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetHealth {
    pub health: f32,
    #[var]
    pub food: u32,
    pub saturation: f32,
}
