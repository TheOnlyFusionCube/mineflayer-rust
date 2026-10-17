use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use uuid::Uuid;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundChatSessionUpdate {
    pub chat_session: RemoteChatSessionData,
}

#[derive(McBuf, Clone, Debug, Eq, PartialEq)]
pub struct RemoteChatSessionData {
    pub session_id: Uuid,
    pub profile_public_key: ProfilePublicKeyData,
}

#[derive(McBuf, Clone, Debug, Eq, PartialEq)]
pub struct ProfilePublicKeyData {
    pub expires_at: u64,
    pub key: Vec<u8>,
    pub key_signature: Vec<u8>,
}
