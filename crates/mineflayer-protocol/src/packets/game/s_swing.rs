use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::packets::game::s_interact::InteractionHand;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSwing {
    pub hand: InteractionHand,
}
