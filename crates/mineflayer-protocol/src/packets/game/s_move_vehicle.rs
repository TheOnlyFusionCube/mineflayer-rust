use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_entity::LookDirection;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundMoveVehicle {
    pub pos: Vec3,
    pub look_direction: LookDirection,
}
