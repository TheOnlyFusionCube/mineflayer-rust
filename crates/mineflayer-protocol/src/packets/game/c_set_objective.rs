use std::io::{self, Cursor, Write};

use mineflayer_buf::McBuf;
use mineflayer_chat::{FormattedText, numbers::NumberFormat};
use mineflayer_core::objectives::ObjectiveCriteria;
use mineflayer_protocol_macros::ClientboundGamePacket;

#[derive(McBuf, ClientboundGamePacket, Clone, Debug, PartialEq)]
pub struct ClientboundSetObjective {
    pub objective_name: String,
    pub method: Method,
}

#[derive(McBuf, Clone, Copy, Debug, PartialEq)]
pub enum MethodKind {
    Add,
    Remove,
    Change,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Method {
    Add {
        display_name: FormattedText,
        render_type: ObjectiveCriteria,
        number_format: NumberFormat,
    },
    Remove,
    Change {
        display_name: FormattedText,
        render_type: ObjectiveCriteria,
        number_format: NumberFormat,
    },
}

impl McBuf for Method {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, mineflayer_buf::BufReadError> {
        let kind = MethodKind::mc_read(buf)?;
        match kind {
            MethodKind::Add => Ok(Method::Add {
                display_name: FormattedText::mc_read(buf)?,
                render_type: ObjectiveCriteria::mc_read(buf)?,
                number_format: NumberFormat::mc_read(buf)?,
            }),
            MethodKind::Remove => Ok(Method::Remove),
            MethodKind::Change => Ok(Method::Change {
                display_name: FormattedText::mc_read(buf)?,
                render_type: ObjectiveCriteria::mc_read(buf)?,
                number_format: NumberFormat::mc_read(buf)?,
            }),
        }
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        match self {
            Method::Add {
                display_name,
                render_type,
                number_format,
            } => {
                MethodKind::Add.mc_write(buf)?;
                display_name.mc_write(buf)?;
                render_type.mc_write(buf)?;
                number_format.mc_write(buf)?;
            }
            Method::Remove => MethodKind::Remove.mc_write(buf)?,
            Method::Change {
                display_name,
                render_type,
                number_format,
            } => {
                MethodKind::Change.mc_write(buf)?;
                display_name.mc_write(buf)?;
                render_type.mc_write(buf)?;
                number_format.mc_write(buf)?;
            }
        }
        Ok(())
    }
}
