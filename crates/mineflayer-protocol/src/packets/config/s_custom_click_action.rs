use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundConfigPacket;
use simdnbt::owned::Nbt;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundCustomClickAction {
    pub id: Identifier,
    pub payload: Nbt,
}
