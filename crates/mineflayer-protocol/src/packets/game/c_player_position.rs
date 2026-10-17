use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

use crate::common::movements::{PositionMoveRotation, RelativeMovements};

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerPosition {
    /// The teleport ID.
    #[var]
    pub id: u32,
    pub change: PositionMoveRotation,
    pub relative: RelativeMovements,
}
