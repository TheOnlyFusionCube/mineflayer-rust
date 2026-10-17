use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_registry::identifier::Identifier;

#[derive(Clone, Debug, McBuf, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetGameRule {
    pub entries: Vec<GameRuleEntry>,
}

#[derive(Clone, Debug, McBuf, PartialEq)]
pub struct GameRuleEntry {
    /// An identifier for a [`GameRule`](mineflayer_registry::builtin::GameRule).
    pub key: Identifier,
    pub value: String,
}
