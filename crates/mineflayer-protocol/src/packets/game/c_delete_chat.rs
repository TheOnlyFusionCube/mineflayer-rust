use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use super::c_player_chat::PackedMessageSignature;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDeleteChat {
    pub signature: PackedMessageSignature,
}
