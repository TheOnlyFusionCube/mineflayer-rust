use mineflayer_buf::McBuf;
use mineflayer_core::entity_id::MinecraftEntityId;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(Clone, Debug, McBuf, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSpectateEntity {
    pub entity_id: MinecraftEntityId,
}
