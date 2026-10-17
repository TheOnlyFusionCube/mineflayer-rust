use std::hash::Hash;

use mineflayer_buf::{McBuf, UnsizedByteArray};
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundLoginPacket;

#[derive(McBuf, ClientboundLoginPacket, Clone, Debug, Hash, PartialEq)]
pub struct ClientboundCustomQuery {
    #[var]
    pub transaction_id: u32,
    pub identifier: Identifier,
    pub data: UnsizedByteArray,
}
