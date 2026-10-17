use mineflayer_buf::McBuf;
use mineflayer_core::bitset::FixedBitSet;
use mineflayer_crypto::signing::MessageSignature;
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundChat {
    #[limit(256)]
    pub message: String,
    pub timestamp: u64,
    pub salt: u64,
    pub signature: Option<MessageSignature>,
    pub last_seen_messages: LastSeenMessagesUpdate,
}

#[derive(McBuf, Clone, Debug, Default, PartialEq)]
pub struct LastSeenMessagesUpdate {
    #[var]
    pub offset: u32,
    pub acknowledged: FixedBitSet<20>,
    pub checksum: u8,
}
