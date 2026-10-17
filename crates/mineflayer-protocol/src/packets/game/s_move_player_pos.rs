use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::common::movements::MoveFlags;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundMovePlayerPos {
    pub pos: Vec3,
    pub flags: MoveFlags,
}
