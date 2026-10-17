use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;
use indexmap::IndexMap;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundCustomReportDetails {
    pub details: IndexMap<String, String>,
}
