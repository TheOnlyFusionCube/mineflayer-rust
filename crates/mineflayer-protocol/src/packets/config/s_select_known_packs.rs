use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundConfigPacket;

#[derive(McBuf, Clone, Debug, PartialEq, ServerboundConfigPacket)]
pub struct ServerboundSelectKnownPacks {
    pub known_packs: Vec<KnownPack>,
}

#[derive(McBuf, Clone, Debug, PartialEq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}
