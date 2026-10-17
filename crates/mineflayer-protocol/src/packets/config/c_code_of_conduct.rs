use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundCodeOfConduct {
    pub code_of_conduct: String,
}
