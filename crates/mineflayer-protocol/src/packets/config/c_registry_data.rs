use mineflayer_buf::McBuf;
use mineflayer_registry::identifier::Identifier;
use mineflayer_protocol_macros::ClientboundConfigPacket;
use simdnbt::owned::NbtCompound;

#[derive(McBuf, ClientboundConfigPacket, Clone, Debug, PartialEq)]
pub struct ClientboundRegistryData {
    pub registry_id: Identifier,
    pub entries: Vec<(Identifier, Option<NbtCompound>)>,
}
