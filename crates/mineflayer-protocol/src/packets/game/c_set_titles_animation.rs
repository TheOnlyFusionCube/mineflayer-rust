use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetTitlesAnimation {
    pub fade_in: u32,
    pub stay: u32,
    pub fade_out: u32,
}
