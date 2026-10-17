use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ClientboundConfigPacket;

use super::s_select_known_packs::KnownPack;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundSelectKnownPacks {
    pub known_packs: Vec<KnownPack>,
}
