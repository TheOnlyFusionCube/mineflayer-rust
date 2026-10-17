use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundPlayerRotation {
    pub y_rot: f32,
    pub relative_y: bool,
    pub x_rot: f32,
    pub relative_x: bool,
}
