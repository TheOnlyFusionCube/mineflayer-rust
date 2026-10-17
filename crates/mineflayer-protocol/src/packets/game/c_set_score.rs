use mineflayer_buf::McBuf;
use mineflayer_chat::{numbers::NumberFormat, FormattedText};
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetScore {
    pub owner: String,
    pub objective_name: String,
    #[var]
    pub score: u32,
    pub display: Option<FormattedText>,
    pub number_format: Option<NumberFormat>,
}
