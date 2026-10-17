use std::hash::Hash;

use mineflayer_buf::{McBuf, UnsizedByteArray};
use mineflayer_protocol_macros::ServerboundLoginPacket;

#[derive(McBuf, Clone, Debug, Hash, PartialEq, ServerboundLoginPacket)]
pub struct ServerboundCustomQueryAnswer {
    #[var]
    pub transaction_id: u32,
    pub data: Option<UnsizedByteArray>,
}
