use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_entity::LookDirection;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMoveVehicle {
    pub pos: Vec3,
    pub look_direction: LookDirection,
}
