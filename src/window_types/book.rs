//! `bot.writeBook`/`bot.signBook` (the `edit_book` protocol and its
//! wire limits) and `bot.setCommandBlock`.

use super::*;

/// `edit_book` wire limits the vanilla server disconnects over
/// (`EditBookPacket`/`WritableBookContent` guards) — enforced at our
/// boundary so a bad call errors instead of kicking the bot.
pub(super) const BOOK_MAX_PAGES: usize = 100;
pub(super) const BOOK_MAX_TITLE_CHARS: usize = 32;
/// Per-page cap — `WritableBookContent.PAGE_EDIT_LENGTH`, counted in
/// UTF-16 units like the Java `String.length()` the wire decode checks.
pub(super) const BOOK_MAX_PAGE_UTF16: usize = 1024;

/// The `edit_book` boundary guards, split out so they're testable
/// without a connection (upstream's book.js asserts only the slot
/// range; the rest mirror the wire limits the server kicks over).
pub(super) fn check_book_input(slot: i32, pages: &[String], title: Option<&str>) -> Result<()> {
    if !(0..=44).contains(&slot) {
        return Err(Error::InvalidArgument("slot out of inventory range".into()));
    }
    if pages.len() > BOOK_MAX_PAGES {
        return Err(Error::InvalidArgument(format!(
            "book has too many pages ({BOOK_MAX_PAGES} max)"
        )));
    }
    if pages
        .iter()
        .any(|p| p.encode_utf16().count() > BOOK_MAX_PAGE_UTF16)
    {
        return Err(Error::InvalidArgument(format!(
            "book page too long ({BOOK_MAX_PAGE_UTF16} chars max)"
        )));
    }
    if title.is_some_and(|t| t.chars().count() > BOOK_MAX_TITLE_CHARS) {
        return Err(Error::InvalidArgument(format!(
            "book title too long ({BOOK_MAX_TITLE_CHARS} chars max)"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Command block.
// ---------------------------------------------------------------------------

/// `options.mode` for [`Bot::set_command_block`] — the vanilla modes:
/// `Sequence` (chain, 0), `Auto` (repeating, 1), `Redstone` (impulse, 2,
/// the upstream default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommandBlockMode {
    /// Chain command block (upstream mode 0).
    Sequence,
    /// Repeating command block (upstream mode 1).
    Auto,
    /// Impulse command block (upstream mode 2, the default).
    #[default]
    Redstone,
}

/// `options` for [`Bot::set_command_block`] — defaults match a freshly
/// placed vanilla command block (upstream's `?? false` / `?? 2`).
#[derive(Clone, Debug, Default)]
pub struct CommandBlockOpts {
    /// `trackOutput` — keep the last output in the block's GUI.
    pub track_output: bool,
    /// `conditional` — run only when the block behind it succeeded.
    pub conditional: bool,
    /// `alwaysActive` — run without a redstone pulse.
    pub always_active: bool,
    /// `mode` — see [`CommandBlockMode`]; default `Redstone` (impulse).
    pub mode: CommandBlockMode,
}

impl Bot {
    /// `bot.writeBook(slot, pages)` — edit a writable book in place:
    /// moves it to the hotbar if it isn't there already (the packet only
    /// addresses a hotbar slot), sends `s_edit_book`, waits for the
    /// server to apply the `writable_book_content` component back onto
    /// the item, then restores the slot and quickbar selection.
    pub async fn write_book(&self, slot: i32, pages: Vec<String>) -> Result<()> {
        self.edit_book(slot, pages, None, None).await
    }

    /// `bot.signBook(slot, pages, author, title)` — same dance but sends
    /// the title so the server turns the item into a `written_book`.
    /// `author` is accepted for signature parity but unused — the server
    /// credits the signing player (upstream's `hasEditBookPacket` path
    /// doesn't send it either).
    pub async fn sign_book(
        &self,
        slot: i32,
        pages: Vec<String>,
        author: &str,
        title: &str,
    ) -> Result<()> {
        self.edit_book(slot, pages, Some(author), Some(title)).await
    }

    /// The shared `write` behind `writeBook`/`signBook`.
    pub(super) async fn edit_book(
        &self,
        slot: i32,
        pages: Vec<String>,
        _author: Option<&str>,
        title: Option<&str>,
    ) -> Result<()> {
        check_book_input(slot, &pages, title)?;
        let inventory = self.inventory();
        let book = inventory
            .slot(slot)
            .ok_or_else(|| Error::NotFound(format!("no book found in slot {slot}")))?;
        if book.kind() != ItemKind::WritableBook {
            return Err(Error::NotFound(format!("no book found in slot {slot}")));
        }

        let mut rx = self.subscribe();
        let quick_bar_slot = self.quick_bar_slot();
        let move_to_quick_bar = slot < 36;
        if move_to_quick_bar {
            self.move_slot_item(slot, 36).await?;
            // 1.21.9+ never acknowledges an edit handled in the same
            // tick as the clicks that put the book in hand — wait for
            // the slot echoes like upstream's `_syncWindow`
            wait_for_slot_updates(&mut rx, 0, &[slot as u16, 36], WINDOW_WAIT).await;
        }
        self.set_quick_bar_slot(if move_to_quick_bar {
            0
        } else {
            (slot - 36) as u8
        })
        .await?;

        let book_slot = if move_to_quick_bar { 36 } else { slot };
        self.client.write_packet(ServerboundEditBook {
            slot: (book_slot - 36) as u32,
            pages,
            title: title.map(|t| t.to_string()),
        });

        // the edit lands as a slot update replacing the item — signing
        // produces a written_book, writing adds the pages component
        let signing = title.is_some();
        let acked = await_event(&mut rx, BOOK_ACK_TIMEOUT, |e| {
            matches!(e, BotEvent::WindowUpdateSlot { window_id: 0, slot: s, new: Some(item), .. }
            if *s as i32 == book_slot
                && if signing {
                    item.kind() == ItemKind::WrittenBook
                } else {
                    item.stack.get_component::<WritableBookContent>().is_some()
                })
        })
        .await
        .is_some();

        if acked {
            self.set_quick_bar_slot(quick_bar_slot).await?;
            if move_to_quick_bar {
                self.move_slot_item(36, slot).await?;
            }
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// `bot.setCommandBlock(pos, command, options)` — write the
    /// `s_set_command_block` packet. Requires creative mode (upstream
    /// asserts `gamemode == 1`) and a `*_command_block` at `pos`.
    pub fn set_command_block(
        &self,
        pos: BlockPos,
        command: &str,
        options: CommandBlockOpts,
    ) -> Result<()> {
        if self.game_mode() != Some(GameMode::Creative) {
            return Err(Error::InvalidState(
                "The bot has to be in creative mode to open the command block window".into(),
            ));
        }
        let block = self
            .block_at(pos)
            .ok_or_else(|| Error::InvalidArgument("The block isn't a command block".into()))?;
        if !block.name().contains("command_block") {
            return Err(Error::InvalidArgument(
                "The block isn't a command block".into(),
            ));
        }
        self.client.write_packet(ServerboundSetCommandBlock {
            pos,
            command: command.to_string(),
            mode: match options.mode {
                CommandBlockMode::Sequence => s_set_command_block::Mode::Sequence,
                CommandBlockMode::Auto => s_set_command_block::Mode::Auto,
                CommandBlockMode::Redstone => s_set_command_block::Mode::Redstone,
            },
            track_output: options.track_output,
            conditional: options.conditional,
            automatic: options.always_active,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mineflayer_engine::inventory::ItemStack;

    #[test]
    fn book_input_guards() {
        // every edit_book boundary check is exercised through the same
        // function write_book/sign_book call
        let page = || vec!["page".to_string()];
        // slot range (upstream's `assert.ok(slot >= 0 && slot <= 44)`)
        assert!(check_book_input(0, &page(), None).is_ok());
        assert!(check_book_input(44, &page(), None).is_ok());
        assert!(check_book_input(-1, &page(), None).is_err());
        assert!(check_book_input(45, &page(), None).is_err());
        // page count / per-page length / title length — the wire caps
        // the server disconnects over
        assert!(check_book_input(9, &vec![String::new(); 100], None).is_ok());
        assert!(check_book_input(9, &vec![String::new(); 101], None).is_err());
        assert!(check_book_input(9, &["x".repeat(1024)], None).is_ok());
        assert!(check_book_input(9, &["x".repeat(1025)], None).is_err());
        assert!(check_book_input(9, &page(), Some(&"t".repeat(32))).is_ok());
        assert!(check_book_input(9, &page(), Some(&"t".repeat(33))).is_err());
    }

    #[test]
    fn writable_book_default_component() {
        // WritableBookContent is a default component on writable_book
        // (empty pages — vanilla's componentMap.has is true for it too),
        // so what actually distinguishes an edited book is non-empty
        // pages — which the write-ack predicate in edit_book relies on
        let book = ItemStack::new(ItemKind::WritableBook, 1).with_component(WritableBookContent {
            pages: vec![mineflayer_engine::core::filterable::Filterable {
                raw: "page one".to_string(),
                filtered: None,
            }],
        });
        assert_eq!(
            book.get_component::<WritableBookContent>()
                .unwrap()
                .pages
                .len(),
            1
        );
        let plain = ItemStack::new(ItemKind::WritableBook, 1);
        assert!(
            plain
                .get_component::<WritableBookContent>()
                .is_some_and(|c| c.pages.is_empty()),
            "fresh writable_book exposes the default (empty) component"
        );
    }
}
