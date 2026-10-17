use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundConfigPacket;

use crate::common::client_information::ClientInformation;

#[derive(McBuf, Clone, Debug, Eq, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundClientInformation {
    pub information: ClientInformation,
}
