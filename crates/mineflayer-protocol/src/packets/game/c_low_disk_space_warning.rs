use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(Clone, Debug, McBuf, PartialEq, ClientboundGamePacket)]
pub struct ClientboundLowDiskSpaceWarning;
