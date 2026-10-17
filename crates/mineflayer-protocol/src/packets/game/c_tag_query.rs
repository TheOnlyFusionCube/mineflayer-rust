use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use simdnbt::owned::NbtTag;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundTagQuery {
    #[var]
    pub transaction_id: u32,
    pub tag: NbtTag,
}
