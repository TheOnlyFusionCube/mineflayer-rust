use mineflayer_buf::McBuf;
use mineflayer_core::position::Vec3;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerLookAt {
    pub from_anchor: Anchor,
    pub pos: Vec3,
    pub entity: Option<AtEntity>,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Feet = 0,
    Eyes = 1,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct AtEntity {
    #[var]
    pub entity: u32,
    pub to_anchor: Anchor,
}
