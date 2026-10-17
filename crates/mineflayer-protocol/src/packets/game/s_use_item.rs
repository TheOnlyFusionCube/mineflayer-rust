use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::packets::game::s_interact::InteractionHand;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundUseItem {
    pub hand: InteractionHand,
    #[var]
    pub seq: u32,
    pub y_rot: f32,
    pub x_rot: f32,
}
