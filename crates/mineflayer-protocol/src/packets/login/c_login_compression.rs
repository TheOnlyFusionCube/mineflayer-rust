use std::hash::Hash;

use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundLoginPacket;

#[derive(McBuf, ClientboundLoginPacket, Clone, Debug, Hash, PartialEq)]
pub struct ClientboundLoginCompression {
    #[var]
    pub compression_threshold: i32,
}
