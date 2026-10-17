use std::io::{self, Cursor, Write};

use mineflayer_buf::McBuf;
use mineflayer_protocol_macros::ServerboundGamePacket;
use mineflayer_registry::identifier::Identifier;

use crate::packets::BufReadError;

#[derive(Clone, Debug, PartialEq, ServerboundGamePacket)]
pub struct ServerboundSeenAdvancements {
    pub action: Action,
    pub tab: Option<Identifier>,
}

#[derive(McBuf, Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    OpenedTab = 0,
    ClosedScreen = 1,
}

impl McBuf for ServerboundSeenAdvancements {
    fn mc_read(buf: &mut Cursor<&[u8]>) -> Result<Self, BufReadError> {
        let action = Action::mc_read(buf)?;
        let tab = if action == Action::OpenedTab {
            Some(Identifier::mc_read(buf)?)
        } else {
            None
        };
        Ok(Self { action, tab })
    }
    fn mc_write(&self, buf: &mut impl Write) -> io::Result<()> {
        self.action.mc_write(buf)?;
        if let Some(tab) = &self.tab {
            tab.mc_write(buf)?;
        }
        Ok(())
    }
}
