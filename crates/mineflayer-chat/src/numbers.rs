//! Contains a few ways to style numbers. At the time of writing, Minecraft only
//! uses this for rendering scoreboard objectives.

#[cfg(feature = "mineflayer-buf")]
use std::io::{self, Cursor, Write};

#[cfg(feature = "mineflayer-buf")]
use mineflayer_buf::McBuf;
#[cfg(feature = "mineflayer-buf")]
use mineflayer_registry::builtin::NumberFormatKind;
use simdnbt::owned::Nbt;

use crate::FormattedText;

#[derive(Clone, Debug, PartialEq)]
pub enum NumberFormat {
    Blank,
    Styled { style: Nbt },
    Fixed { value: FormattedText },
}

#[cfg(feature = "mineflayer-buf")]
impl McBuf for NumberFormat {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, mineflayer_buf::BufReadError> {
        let kind = NumberFormatKind::mc_read(buf)?;
        match kind {
            NumberFormatKind::Blank => Ok(NumberFormat::Blank),
            NumberFormatKind::Styled => Ok(NumberFormat::Styled {
                style: simdnbt::owned::read(buf)?,
            }),
            NumberFormatKind::Fixed => Ok(NumberFormat::Fixed {
                value: FormattedText::mc_read(buf)?,
            }),
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            NumberFormat::Blank => NumberFormatKind::Blank.mc_write(buf)?,
            NumberFormat::Styled { style } => {
                NumberFormatKind::Styled.mc_write(buf)?;
                style.mc_write(buf)?;
            }
            NumberFormat::Fixed { value } => {
                NumberFormatKind::Fixed.mc_write(buf)?;
                value.mc_write(buf)?;
            }
        }
        Ok(())
    }
}
