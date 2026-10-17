use mineflayer_buf::McBuf;
use mineflayer_chat::FormattedText;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

/// Used to send a respawn screen.
#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerCombatKill {
    #[var]
    pub player_id: MinecraftEntityId,
    pub message: FormattedText,
}
