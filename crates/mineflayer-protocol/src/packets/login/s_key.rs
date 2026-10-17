use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundLoginPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundLoginPacket)]
pub struct ServerboundKey {
    pub key_bytes: Vec<u8>,
    pub encrypted_challenge: Vec<u8>,
}
