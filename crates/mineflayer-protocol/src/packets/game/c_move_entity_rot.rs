use mineflayer_buf::McBuf;
use mineflayer_core::entity_id::MinecraftEntityId;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::packets::game::c_move_entity_pos_rot::CompactLookDirection;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundMoveEntityRot {
    #[var]
    pub entity_id: MinecraftEntityId,
    pub look_direction: CompactLookDirection,
    pub on_ground: bool,
}
