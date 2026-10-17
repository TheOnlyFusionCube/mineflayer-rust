use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::common::movements::MoveFlags;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundMovePlayerStatusOnly {
    pub flags: MoveFlags,
}
