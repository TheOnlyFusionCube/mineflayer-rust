use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

/// Unused by the client in vanilla.
#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerCombatEnd {
    #[var]
    pub duration: u32,
}
