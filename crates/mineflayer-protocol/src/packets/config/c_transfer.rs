use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundTransfer {
    pub host: String,
    #[var]
    pub port: u32,
}
