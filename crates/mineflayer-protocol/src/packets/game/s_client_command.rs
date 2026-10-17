use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundClientCommand {
    pub action: Action,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum Action {
    PerformRespawn = 0,
    RequestStats = 1,
    RequestGameruleValues = 2,
}
