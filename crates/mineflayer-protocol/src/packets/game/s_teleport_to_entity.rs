use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use uuid::Uuid;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundTeleportToEntity {
    pub uuid: Uuid,
}
