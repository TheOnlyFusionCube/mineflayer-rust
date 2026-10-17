use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetBorderLerpSize {
    pub old_size: f64,
    pub new_size: f64,
    #[var]
    pub lerp_time: u64,
}
