use std::io::{self, Cursor, Write};

use mineflayer_buf::{McBuf, BufReadError};
use mineflayer_core::{bitset::FixedBitSet, position::BlockPos};
use mineflayer_protocol_macros::ServerboundGamePacket;

#[derive(Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSetCommandBlock {
    pub pos: BlockPos,
    pub command: String,
    pub mode: Mode,

    pub track_output: bool,
    pub conditional: bool,
    pub automatic: bool,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Sequence = 0,
    Auto = 1,
    Redstone = 2,
}

impl McBuf for ServerboundSetCommandBlock {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let pos = BlockPos::mc_read(buf)?;
        let command = String::mc_read(buf)?;
        let mode = Mode::mc_read(buf)?;

        let set = FixedBitSet::<3>::mc_read(buf)?;
        Ok(Self {
            pos,
            command,
            mode,
            track_output: set.index(0),
            conditional: set.index(1),
            automatic: set.index(2),
        })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.pos.mc_write(buf)?;
        self.command.mc_write(buf)?;
        self.mode.mc_write(buf)?;

        let mut set = FixedBitSet::<3>::new();
        if self.track_output {
            set.set(0);
        }
        if self.conditional {
            set.set(1);
        }
        if self.automatic {
            set.set(2);
        }
        set.mc_write(buf)
    }
}
