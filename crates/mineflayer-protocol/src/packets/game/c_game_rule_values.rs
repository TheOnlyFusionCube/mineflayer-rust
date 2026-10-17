use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::builtin::GameRule;
use indexmap::IndexMap;

#[derive(Clone, Debug, McBuf, PartialEq, ClientboundGamePacket)]
pub struct ClientboundGameRuleValues {
    pub values: IndexMap<GameRule, String>,
}
