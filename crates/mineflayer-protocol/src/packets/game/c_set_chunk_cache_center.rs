use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetChunkCacheCenter {
    #[var]
    pub x: i32,
    #[var]
    pub z: i32,
}
