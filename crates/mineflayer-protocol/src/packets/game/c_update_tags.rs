use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::tags::TagMap;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateTags {
    pub tags: TagMap,
}
