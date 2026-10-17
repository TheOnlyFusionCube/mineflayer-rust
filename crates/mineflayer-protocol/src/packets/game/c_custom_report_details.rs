use std::collections::HashMap;

use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundCustomReportDetails {
    // the engine doesn't implement max lengths yet

    // max length = 32
    // key string is limited to 128 bytes
    // value string is limited to 4096 bytes
    pub details: HashMap<String, String>,
}
