use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use super::s_interact::InteractionHand;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundOpenBook {
    pub hand: InteractionHand,
}
