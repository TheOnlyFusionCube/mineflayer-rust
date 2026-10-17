use mineflayer_buf::McBuf;
use mineflayer_core::{delta::PositionDelta8, entity_id::MinecraftEntityId};
use mineflayer_entity::LookDirection;
use mineflayer_protocol_macros::ClientboundGamePacket;

/// This packet is sent by the server when an entity moves less then 8 blocks.
#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMoveEntityPosRot {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub delta: PositionDelta8,
    pub look_direction: CompactLookDirection,
    pub on_ground: bool,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq, Default)]
pub struct CompactLookDirection {
    pub y_rot: i8,
    pub x_rot: i8,
}

impl From<CompactLookDirection> for LookDirection {
    fn from(l: CompactLookDirection) -> Self {
        LookDirection::new(
            (l.y_rot as i32 * 360) as f32 / 256.,
            (l.x_rot as i32 * 360) as f32 / 256.,
        )
    }
}
