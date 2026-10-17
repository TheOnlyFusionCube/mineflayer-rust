use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;

use crate::common::tags::TagMap;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateTags {
    pub tags: TagMap,
}
