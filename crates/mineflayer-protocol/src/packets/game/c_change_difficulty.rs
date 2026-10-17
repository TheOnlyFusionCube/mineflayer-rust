use mineflayer_buf::McBuf;
use mineflayer_core::difficulty::Difficulty;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundChangeDifficulty {
    pub difficulty: Difficulty,
    pub locked: bool,
}
