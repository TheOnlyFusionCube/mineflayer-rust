use mineflayer_buf::McBuf;
use mineflayer_core::difficulty::Difficulty;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundChangeDifficulty {
    pub difficulty: Difficulty,
}
