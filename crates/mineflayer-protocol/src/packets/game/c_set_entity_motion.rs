use mineflayer_buf::McBuf;
use mineflayer_core::delta::LpVec3;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_core::entity_id::MinecraftEntityId;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetEntityMotion {
    #[var]
    pub id: MinecraftEntityId,
    pub delta: LpVec3,
}
