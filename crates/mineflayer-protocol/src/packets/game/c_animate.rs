use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundAnimate {
    #[var]
    pub id: MinecraftEntityId,
    pub action: AnimationAction,
}

// minecraft actually uses a u8 for this, but a varint still works and makes it
// so i don't have to add a special handler
#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum AnimationAction {
    SwingMainHand = 0,
    Hurt = 1,
    WakeUp = 2,
    SwingOffHand = 3,
    CriticalHit = 4,
    MagicCriticalHit = 5,
}
