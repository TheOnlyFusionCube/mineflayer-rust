use mineflayer_buf::McBuf;
use mineflayer_core::position::BlockPos;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::BlockEntityKind;
use simdnbt::owned::Nbt;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundBlockEntityData {
    pub pos: BlockPos,
    pub block_entity_type: BlockEntityKind,
    pub tag: Nbt,
}
