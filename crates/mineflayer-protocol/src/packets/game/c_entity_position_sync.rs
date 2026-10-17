use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

use crate::common::movements::PositionMoveRotation;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundEntityPositionSync {
    #[var]
    pub id: MinecraftEntityId,
    pub values: PositionMoveRotation,
    pub on_ground: bool,
}
