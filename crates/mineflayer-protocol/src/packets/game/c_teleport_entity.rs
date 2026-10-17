use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

use crate::common::movements::{PositionMoveRotation, RelativeMovements};

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundTeleportEntity {
    #[var]
    pub id: MinecraftEntityId,
    pub change: PositionMoveRotation,
    pub relative: RelativeMovements,
    pub on_ground: bool,
}
