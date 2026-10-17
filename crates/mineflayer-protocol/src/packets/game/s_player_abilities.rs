use std::io::{self, Cursor, Write};

use mineflayer_buf::McBuf;
use mineflayer_core::bitset::FixedBitSet;
use mineflayer_protocol_macros::ServerboundGamePacket;

use crate::packets::BufReadError;

#[derive(Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundPlayerAbilities {
    pub is_flying: bool,
}

impl McBuf for ServerboundPlayerAbilities {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let set = FixedBitSet::<2>::mc_read(buf)?;
        Ok(Self {
            is_flying: set.index(1),
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        let mut set = FixedBitSet::<2>::new();
        if self.is_flying {
            set.set(1);
        }
        set.mc_write(buf)
    }
}
