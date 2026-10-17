use mineflayer_buf::McBuf;
use mineflayer_core::{direction::Direction, position::BlockPos};
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundPlayerAction {
    pub action: Action,
    pub pos: BlockPos,
    pub direction: Direction,
    #[var]
    pub seq: u32,
}

#[derive(McBuf, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    StartDestroyBlock = 0,
    AbortDestroyBlock = 1,
    StopDestroyBlock = 2,
    DropAllItems = 3,
    DropItem = 4,
    ReleaseUseItem = 5,
    SwapItemWithOffhand = 6,
}
