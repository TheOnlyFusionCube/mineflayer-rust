use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;

use crate::common::server_links::ServerLinkEntry;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundServerLinks {
    pub links: Vec<ServerLinkEntry>,
}
