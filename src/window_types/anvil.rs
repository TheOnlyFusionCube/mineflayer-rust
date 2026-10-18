//! `bot.openAnvil` and the anvil window: `combine` and `rename`.

use super::*;

// ---------------------------------------------------------------------------
// Anvil.
// ---------------------------------------------------------------------------

/// `bot.openAnvil` result — combine/rename over the anvil window.
/// [`Deref`]s to the [`Window`].
#[derive(Clone)]
pub struct Anvil {
    window: Window,
    bot: Bot,
}

impl Deref for Anvil {
    type Target = Window;
    fn deref(&self) -> &Window {
        &self.window
    }
}

/// Upstream `err('Name is too long.')` guard — names over 35 chars are
/// rejected before any clicking.
pub(super) fn check_anvil_name(name: &str) -> Result<()> {
    if name.chars().count() > 35 {
        return Err(Error::InvalidArgument("Name is too long.".into()));
    }
    Ok(())
}

impl Anvil {
    /// The server's repair/rename level cost (`container_set_data` 0) —
    /// `None` until the first report. The upstream `Item.anvil` cost
    /// preview has no engine equivalent, so the checks below read this.
    pub fn repair_cost(&self) -> Option<i32> {
        self.bot
            .shared
            .window_data
            .lock()
            .data(self.window.id(), ANVIL_COST)
            .map(|v| v as i32)
    }

    /// `anvil.combine(itemOne, itemTwo, name)` — place both items in the
    /// anvil, optionally rename, take the result, and let the xp update
    /// land. Tries the swapped order when the given order produces no
    /// affordable result (upstream picks the cheaper of the two orders
    /// client-side; we can only compare server-reported costs, so the
    /// given order wins when it already works — see api-parity).
    pub async fn combine(
        &self,
        item_one: &Item,
        item_two: &Item,
        name: Option<&str>,
    ) -> Result<()> {
        if let Some(name) = name {
            check_anvil_name(name).inspect_err(|_| self.window.close())?;
        }
        // upstream `assert.ok(itemOne && itemTwo)`
        if item_one.stack.is_empty() || item_two.stack.is_empty() {
            self.window.close();
            return Err(Error::InvalidArgument("Not anvil-able (empty item)".into()));
        }
        let creative = self.bot.game_mode() == Some(GameMode::Creative);
        let mut rx = self.bot.subscribe();
        self.put_in_anvil(item_one, item_two).await?;
        // the name goes in before the check — upstream's client-side
        // cost computation includes it, and ours must read the server's
        // post-rename cost the same way (the text persists across an
        // input swap, so no resend is needed)
        self.send_item_name(name.unwrap_or("")).await;

        let first_ok = self.wait_and_check(creative, &mut rx).await;
        if !first_ok && self.swap_inputs().await.is_ok() {
            // the reverse direction can work (or be cheaper) when the
            // given one doesn't
            let second_ok = self.wait_and_check(creative, &mut rx).await;
            if !second_ok {
                self.window.close();
                return Err(self.anvil_fail());
            }
        } else if !first_ok {
            self.window.close();
            return Err(self.anvil_fail());
        }

        self.take_result(creative, &mut rx).await
    }

    /// `anvil.rename(item, name)` — rename a single item via the anvil.
    pub async fn rename(&self, item: &Item, name: &str) -> Result<()> {
        check_anvil_name(name).inspect_err(|_| self.window.close())?;
        if item.stack.is_empty() {
            self.window.close();
            return Err(Error::InvalidArgument(
                "Not valid rename (empty item)".into(),
            ));
        }
        let creative = self.bot.game_mode() == Some(GameMode::Creative);
        let mut rx = self.bot.subscribe();
        let player = self.window.player_range();
        transfer(TransferOpts {
            window: Some(self.window.clone()),
            kind: item.kind(),
            metadata: item.metadata(),
            nbt: Some(item.component_patch().clone()),
            count: Some(item.count()),
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: 0,
            dest_end: Some(1),
        })
        .await?;
        // vanilla sends the empty name after the first input, twice on
        // the modern `name_item` packet (upstream `sendItemName('')` x2)
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.send_item_name(name).await;

        if !self.wait_and_check(creative, &mut rx).await {
            self.window.close();
            return Err(self.anvil_fail());
        }
        self.take_result(creative, &mut rx).await
    }

    /// Upstream `putInAnvil` — first input, the two empty `name_item`
    /// packets vanilla sends, then the second input.
    pub(super) async fn put_in_anvil(&self, item_one: &Item, item_two: &Item) -> Result<()> {
        self.put_something(0, item_one).await?;
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.bot.client.write_packet(ServerboundRenameItem {
            name: String::new(),
        });
        self.put_something(1, item_two).await
    }

    /// Upstream `putSomething` — transfer a matching stack from the
    /// player region into an anvil input slot.
    pub(super) async fn put_something(&self, dest: i32, item: &Item) -> Result<()> {
        let player = self.window.player_range();
        transfer(TransferOpts {
            window: Some(self.window.clone()),
            kind: item.kind(),
            metadata: item.metadata(),
            nbt: Some(item.component_patch().clone()),
            count: Some(item.count()),
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: dest,
            dest_end: Some(dest + 1),
        })
        .await
    }

    /// Swap the two anvil inputs in place (slot 0 ↔ 1 via the
    /// pick-up/swap/put-back choreography of `moveSlotItem`).
    pub(super) async fn swap_inputs(&self) -> Result<()> {
        self.bot.move_slot_item(0, 1).await
    }

    /// Wait for the server to resync the result slot or cost after an
    /// input change, then report whether the current inputs produce an
    /// affordable result. The cost packet only goes out when the value
    /// changes, so the wait is bounded and the latest stored value is
    /// read afterwards.
    pub(super) async fn wait_and_check(
        &self,
        creative: bool,
        rx: &mut broadcast::Receiver<BotEvent>,
    ) -> bool {
        let id = self.window.id();
        let _ = await_event(rx, ANVIL_COST_WAIT, |e| {
            matches!(
                e,
                BotEvent::WindowDataUpdate { window_id, property: ANVIL_COST, .. }
                    | BotEvent::WindowUpdateSlot { window_id, slot: 2, .. }
                    if *window_id == id
            )
        })
        .await;
        self.window.slot(2).is_some()
            && (creative
                || self
                    .repair_cost()
                    .is_some_and(|c| self.bot.experience().level >= c as u32))
    }

    /// The failure to report when the result never becomes affordable —
    /// `Not anvil-able` when the server never produced a result at all,
    /// else the xp complaint (upstream's two `err()` sites).
    pub(super) fn anvil_fail(&self) -> Error {
        if self.window.slot(2).is_some() {
            Error::InvalidState("Player does not have enough xp to do action, cancelling.".into())
        } else {
            Error::InvalidArgument("Not anvil-able (in either direction), cancelling.".into())
        }
    }

    /// Upstream `addCustomName` — the packet is sent for each name
    /// prefix like vanilla typing, 50 ms apart.
    pub(super) async fn send_item_name(&self, name: &str) {
        if name.is_empty() {
            return;
        }
        for i in 1..=name.chars().count() {
            let prefix: String = name.chars().take(i).collect();
            self.bot
                .client
                .write_packet(ServerboundRenameItem { name: prefix });
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// `putAway(2)` the result into the inventory, then wait for the xp
    /// deduction upstream waits on (`once(bot, 'experience')`, skipped
    /// in creative).
    pub(super) async fn take_result(
        &self,
        creative: bool,
        rx: &mut broadcast::Receiver<BotEvent>,
    ) -> Result<()> {
        self.bot.put_away(TRADE_RESULT).await?;
        if !creative {
            let _ = await_event(rx, WINDOW_WAIT, |e| matches!(e, BotEvent::Experience)).await;
        }
        Ok(())
    }
}

impl Bot {
    /// `bot.openAnvil(block)` — right-click the block and verify the
    /// `anvil` menu kind (covers damaged/chipped anvils — they all map
    /// to the same menu).
    pub async fn open_anvil(&self, block: &Block) -> Result<Anvil> {
        let window = self.open_block(block).await?;
        match window.menu_kind() {
            Some(MenuKind::Anvil) => Ok(Anvil {
                window,
                bot: self.clone(),
            }),
            other => {
                window.close();
                Err(Error::InvalidArgument(format!(
                    "Not a anvil-like window: {other:?}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anvil_name_limit() {
        assert!(check_anvil_name(&"x".repeat(35)).is_ok());
        assert!(check_anvil_name(&"x".repeat(36)).is_err());
    }
}
