use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

/// Unused in vanilla.
#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerCombatEnter;
