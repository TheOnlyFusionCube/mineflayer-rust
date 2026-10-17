use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::common::client_information::ClientInformation;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundClientInformation {
    pub client_information: ClientInformation,
}
