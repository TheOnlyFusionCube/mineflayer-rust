use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundCustomChatCompletions {
    pub action: Action,
    pub entries: Vec<String>,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Add = 0,
    Remove = 1,
    Set = 2,
}
