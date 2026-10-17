use std::hash::Hash;

use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundHandshakePacket;

use crate::packets::ClientIntention;

#[derive(McBuf, Clone, Debug, Hash, PartialEq, ServerboundHandshakePacket)]
pub struct ServerboundIntention {
    #[var]
    pub protocol_version: i32,
    pub hostname: String,
    pub port: u16,
    pub intention: ClientIntention,
}
