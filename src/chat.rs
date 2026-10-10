//! Chat — the U1 slice: just `bot.chat`, needed by the lifecycle live test.
//! Everything else (patterns, whisper, awaitMessage, tabComplete) is the
//! chat unit's.

use crate::{Bot, Error, Result};

impl Bot {
    /// `bot.chat(message)` — send a chat message, or a command when it starts
    /// with `/` (azalea's `chat` handles the split).
    ///
    /// Errors when not in-game, matching upstream throwing before `login` or
    /// after an early disconnect. Long-message splitting lands with the chat
    /// unit.
    pub fn chat(&self, message: &str) -> Result<()> {
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        self.client.chat(message);
        Ok(())
    }
}
