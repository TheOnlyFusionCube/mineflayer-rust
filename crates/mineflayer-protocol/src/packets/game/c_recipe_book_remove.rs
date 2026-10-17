use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundRecipeBookRemove {
    #[var]
    pub recipes: Vec<u32>,
}
