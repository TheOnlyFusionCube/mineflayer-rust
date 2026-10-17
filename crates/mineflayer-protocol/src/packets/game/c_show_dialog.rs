use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::{Holder, data::Dialog};
use simdnbt::owned::Nbt;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundShowDialog {
    pub dialog: Holder<Dialog, Nbt>,
}
