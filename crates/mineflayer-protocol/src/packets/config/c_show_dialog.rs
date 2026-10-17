use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;
use simdnbt::owned::Nbt;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundShowDialog {
    pub dialog: Nbt,
}
