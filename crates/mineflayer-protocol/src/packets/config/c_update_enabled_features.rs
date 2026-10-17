use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundConfigPacket;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundUpdateEnabledFeatures {
    pub features: Vec<Identifier>,
}
