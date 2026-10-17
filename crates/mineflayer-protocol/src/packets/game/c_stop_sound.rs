use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::bitset::FixedBitSet;
use mineflayer_protocol_macros::ClientboundGamePacket;
use mineflayer_registry::identifier::Identifier;

use super::c_sound::SoundSource;

#[derive(ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundStopSound {
    pub source: Option<SoundSource>,
    pub name: Option<Identifier>,
}

impl McBuf for ClientboundStopSound {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let set = FixedBitSet::<2>::mc_read(buf)?;
        let source = if set.index(0) {
            Some(SoundSource::mc_read(buf)?)
        } else {
            None
        };
        let name = if set.index(1) {
            Some(Identifier::mc_read(buf)?)
        } else {
            None
        };

        Ok(Self { source, name })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut set = FixedBitSet::<2>::new();
        if self.source.is_some() {
            set.set(0);
        }
        if self.name.is_some() {
            set.set(1);
        }
        set.mc_write(buf)?;
        if let Some(source) = &self.source {
            source.mc_write(buf)?;
        }
        if let Some(name) = &self.name {
            name.mc_write(buf)?;
        }
        Ok(())
    }
}
