use mineflayer_buf::McBuf;
use mineflayer_core::game_type::GameMode;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundChangeGameMode {
    pub mode: GameMode,
}
