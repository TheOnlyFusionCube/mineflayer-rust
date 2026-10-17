use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, McBufVar};
use mineflayer_core::{entity_id::MinecraftEntityId, position::Vec3};
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundDamageEvent {
    #[var]
    pub entity_id: MinecraftEntityId,
    #[var]
    pub source_type_id: u32,
    pub source_cause_id: OptionalEntityId,
    pub source_direct_id: OptionalEntityId,
    pub source_position: Option<Vec3>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptionalEntityId(pub Option<u32>);
impl McBuf for OptionalEntityId {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, mineflayer_buf::BufReadError> {
        match u32::mc_read_var(buf)? {
            0 => Ok(OptionalEntityId(None)),
            id => Ok(OptionalEntityId(Some(id - 1))),
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self.0 {
            Some(id) => (id + 1).mc_write_var(buf),
            None => 0u32.mc_write_var(buf),
        }
    }
}
