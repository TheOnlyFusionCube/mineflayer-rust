use mineflayer_auth::game_profile::GameProfile;
use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundLoginPacket;

#[derive(McBuf, ClientboundLoginPacket, Clone, Debug, PartialEq)]
pub struct ClientboundLoginFinished {
    pub game_profile: GameProfile,
}
