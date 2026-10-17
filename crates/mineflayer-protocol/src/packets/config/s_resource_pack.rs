use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundConfigPacket;
use uuid::Uuid;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundResourcePack {
    pub id: Uuid,
    pub action: Action,
}

#[derive(McBuf, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    SuccessfullyLoaded = 0,
    Declined = 1,
    FailedDownload = 2,
    Accepted = 3,
    InvalidUrl = 4,
    FailedReload = 5,
    Discarded = 6,
}
