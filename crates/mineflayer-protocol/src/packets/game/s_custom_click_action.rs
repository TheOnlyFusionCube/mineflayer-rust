use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ServerboundGamePacket;
use simdnbt::owned::Nbt;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundCustomClickAction {
    pub id: Identifier,
    pub payload: Nbt,
}
