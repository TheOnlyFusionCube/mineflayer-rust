use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundPlayerCommand {
    #[var]
    pub id: MinecraftEntityId,
    pub action: Action,
    #[var]
    pub data: u32,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum Action {
    StopSleeping,
    StartSprinting,
    StopSprinting,
    StartRidingJump,
    StopRidingJump,
    OpenInventory,
    StartFallFlying,
}
