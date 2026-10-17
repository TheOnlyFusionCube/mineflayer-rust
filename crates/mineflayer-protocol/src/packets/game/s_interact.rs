use mineflayer_buf::McBuf;
use mineflayer_core::{delta::LpVec3, entity_id::MinecraftEntityId};
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundInteract {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub hand: InteractionHand,
    pub location: LpVec3,
    /// Whether the player is sneaking.
    pub using_secondary_action: bool,
}

#[derive(McBuf, Clone, Copy, Debug, Default, PartialEq)]
pub enum InteractionHand {
    #[default]
    MainHand = 0,
    OffHand = 1,
}
