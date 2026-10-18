//! Inventory reads, item use, equipment, and window clicks — ports of
//! `inventory.js`, `simple_inventory.js`, and the openers of `chest.js`.
//!
//! Window state lives in [`crate::windows`]; this module is the `Bot`
//! method surface over it. Click choreography (equip, transfer, putAway,
//! toss) follows upstream `inventory.js`/`simple_inventory.js` closely;
//! where upstream clicks through `clickWindow` we send the equivalent
//! The engine [`ClickOperation`] on the same window.
//!
//! A method here is `async` only when it waits for something (a server
//! reply, a tick, a window event). `update_held_item`, `close_window`,
//! `activate_item`, and `deactivate_item` are synchronous upstream and
//! only write a packet or emit an event, so they are plain `fn`.

use std::{ops::Range, time::Duration};

use mineflayer_engine::{
    BlockPos,
    core::{direction::Direction, game_type::GameMode},
    entity::LookDirection,
    inventory::{
        DataComponentPatch, ItemStack,
        operations::{
            ClickOperation, ClickType, CloneClick, PickupClick, QuickMoveClick, SwapClick,
            ThrowClick,
        },
    },
    protocol::packets::game::{
        s_container_click::{HashedStack, ServerboundContainerClick},
        s_player_action::{self, ServerboundPlayerAction},
        s_use_item::ServerboundUseItem,
    },
    registry::builtin::{BlockKind, ItemKind, MenuKind},
};
use tokio::sync::{broadcast, oneshot};

use crate::{
    Bot, BotEvent, Error, Result,
    bot::wait::{await_event, await_map},
    op::{Op, OpSlot},
    windows::{Item, Window, find_item_in, first_empty_in, held_item},
    world::Block,
};

/// `bot.QUICK_BAR_START` — the absolute player-window slot where the
/// 9-slot hotbar begins (upstream 36).
mod transfer;

pub use transfer::TransferOpts;
use transfer::put_selected;

/// The running eat: a newer eat cancels it, and the finish signal ends it.
#[derive(Debug, Default)]
pub(crate) struct Eating {
    op: OpSlot,
    finished: Option<oneshot::Sender<()>>,
}

impl Eating {
    /// End the eat as superseded.
    pub(crate) fn cancel(&mut self) {
        self.op.cancel();
        self.finished = None;
    }

    /// The server or the held item ended the eat.
    pub(crate) fn finish(&mut self) {
        if let Some(finished) = self.finished.take() {
            let _ = finished.send(());
        }
    }
}

/// How an eat wait ended.
pub(crate) enum EatEnd {
    Finished,
    /// A newer eat started.
    Superseded,
    Timeout,
    Disconnected,
}
pub(crate) use transfer::transfer;

pub const QUICK_BAR_START: usize = 36;

/// Upstream `ALWAYS_CONSUMABLES` — items that can be "eaten" even at
/// full hunger.
const ALWAYS_CONSUMABLES: &[ItemKind] = &[
    ItemKind::Potion,
    ItemKind::MilkBucket,
    ItemKind::EnchantedGoldenApple,
    ItemKind::GoldenApple,
];

/// `bot.equip(item, destination)` targets — the upstream destination
/// strings as an enum. `hand` tracks the quickbar; the rest are fixed
/// player-window slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquipDest {
    /// `"head"` → slot 5.
    Head,
    /// `"torso"` → slot 6.
    Torso,
    /// `"legs"` → slot 7.
    Legs,
    /// `"feet"` → slot 8.
    Feet,
    /// `"hand"` → `QUICK_BAR_START + quick_bar_slot` (resolved per call).
    Hand,
    /// `"off-hand"` → slot 45.
    OffHand,
}

/// Fixed destination slots (upstream `armorSlots`). `Hand` resolves
/// through the quickbar selection instead — [`Bot::equipment_dest_slot`].
pub(crate) fn dest_fixed_slot(dest: EquipDest) -> Option<u16> {
    match dest {
        EquipDest::Head => Some(5),
        EquipDest::Torso => Some(6),
        EquipDest::Legs => Some(7),
        EquipDest::Feet => Some(8),
        EquipDest::OffHand => Some(45),
        EquipDest::Hand => None,
    }
}

/// `bot.openContainer` accepts a block (chest/dispenser/…) or an entity
/// (minecart chest, llama, …) — the upstream `openContainer(blockOrEntity)`
/// shape.
#[derive(Clone, Debug)]
pub enum ContainerTarget {
    /// A container block: position + kind for upstream's allowed-list
    /// pre-check (chest.js throws before opening other blocks).
    Block { pos: BlockPos, kind: BlockKind },
    /// An entity reference.
    Entity(mineflayer_engine::EntityRef),
}

impl From<&Block> for ContainerTarget {
    fn from(block: &Block) -> Self {
        ContainerTarget::Block {
            pos: block.pos,
            kind: block.kind(),
        }
    }
}

impl From<Block> for ContainerTarget {
    fn from(block: Block) -> Self {
        (&block).into()
    }
}

impl From<mineflayer_engine::EntityRef> for ContainerTarget {
    fn from(entity: mineflayer_engine::EntityRef) -> Self {
        ContainerTarget::Entity(entity)
    }
}

impl From<&mineflayer_engine::EntityRef> for ContainerTarget {
    fn from(entity: &mineflayer_engine::EntityRef) -> Self {
        ContainerTarget::Entity(entity.clone())
    }
}

/// The blocks whose windows are upstream chest.js `allowedWindowTypes` —
/// the pre-open check for [`ContainerTarget::Block`].
fn is_container_block(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::Chest
            | BlockKind::TrappedChest
            | BlockKind::EnderChest
            | BlockKind::Dispenser
            | BlockKind::Dropper
            | BlockKind::Hopper
            | BlockKind::Barrel
    ) || mineflayer_engine::registry::tags::blocks::SHULKER_BOXES.contains(&kind)
}

/// Upstream `allowedWindowTypes` rendered as the resulting [`MenuKind`]
/// set: chests/barrels/ender chests/trapped chests arrive as
/// `generic_9x*`, droppers/dispensers as `generic_3x3`, plus hopper and
/// shulker box menus.
fn is_container_menu(kind: MenuKind) -> bool {
    matches!(
        kind,
        MenuKind::Generic9x1
            | MenuKind::Generic9x2
            | MenuKind::Generic9x3
            | MenuKind::Generic9x4
            | MenuKind::Generic9x5
            | MenuKind::Generic9x6
            | MenuKind::Generic3x3
            | MenuKind::Hopper
            | MenuKind::ShulkerBox
    )
}

/// `(mouseButton, mode)` → the engine click operation, upstream's
/// `clickWindow` mode dispatch. `slot` is the absolute window slot
/// (`-999` is handled in [`Bot::click_window`], which sends the outside
/// clicks).
pub(crate) fn click_operation(slot: u16, mouse_button: u8, mode: u8) -> Result<ClickOperation> {
    Ok(match mode {
        0 => match mouse_button {
            0 => PickupClick::Left { slot: Some(slot) }.into(),
            1 => PickupClick::Right { slot: Some(slot) }.into(),
            b => return Err(Error::InvalidArgument(format!("invalid mouse button {b}"))),
        },
        1 => match mouse_button {
            0 => QuickMoveClick::Left { slot }.into(),
            1 => QuickMoveClick::Right { slot }.into(),
            b => return Err(Error::InvalidArgument(format!("invalid mouse button {b}"))),
        },
        2 => SwapClick {
            source_slot: slot,
            target_slot: mouse_button,
        }
        .into(),
        // mode 3 is middle-click/clone; upstream passes `mode` through
        // to the wire raw (the mouse_button is unused by click_type 3)
        3 => CloneClick { slot }.into(),
        4 => match mouse_button {
            0 => ThrowClick::Single { slot }.into(),
            1 => ThrowClick::All { slot }.into(),
            b => return Err(Error::InvalidArgument(format!("invalid mouse button {b}"))),
        },
        // modes 5 (quick-craft drag) and 6 (pickup-all double click)
        // are multi-packet gestures upstream sends raw; nothing in the
        // ported API emits them.
        5 | 6 => {
            return Err(Error::Unsupported(format!("unsupported mouse mode {mode}")));
        }
        m => return Err(Error::InvalidArgument(format!("invalid mode {m}"))),
    })
}

impl Bot {
    /// `f` on the bot's window state; `None` once the client entity is
    /// gone.
    pub(crate) fn windows<R>(
        &self,
        f: impl FnOnce(&mut crate::windows::WindowTracker) -> R,
    ) -> Option<R> {
        crate::bot::with_component_mut(&self.client, f)
    }

    /// Start an eat, which cancels the eat it supersedes (upstream's
    /// `eatingTask.cancel`). The receiver resolves on the finish signal.
    pub(crate) fn start_eat(&self) -> Option<(Op, oneshot::Receiver<()>)> {
        self.windows(|t| {
            let op = t.eat.op.start(&self.root);
            let (finished, on_finish) = oneshot::channel();
            t.eat.finished = Some(finished);
            (op, on_finish)
        })
    }

    /// Wait for the eat `op` to end, and release it.
    pub(crate) async fn wait_eat(
        &self,
        op: &Op,
        on_finish: oneshot::Receiver<()>,
        timeout: Duration,
    ) -> EatEnd {
        let end = tokio::select! {
            biased;
            _ = self.root.cancelled() => EatEnd::Disconnected,
            _ = op.cancelled() => EatEnd::Superseded,
            finished = on_finish => match finished {
                Ok(()) => EatEnd::Finished,
                Err(_) => EatEnd::Superseded,
            },
            _ = tokio::time::sleep(timeout) => EatEnd::Timeout,
        };
        self.windows(|t| {
            if t.eat.op.finish(op) {
                t.eat.finished = None;
            }
        });
        end
    }

    /// `bot.inventory` — the player's own inventory as a [`Window`] (id
    /// 0). Always available; a fresh handle per call (upstream keeps one
    /// object — slot reads are equivalent since both read live state).
    pub fn inventory(&self) -> Window {
        Window::player_inventory(self.client.clone(), self.shared.events.clone())
    }

    /// `bot.currentWindow` — the open non-inventory window, `None` when
    /// only the player inventory is open.
    pub fn current_window(&self) -> Option<Window> {
        self.windows(|t| t.current.clone())
            .flatten()
            .map(|shared| Window { shared })
    }

    /// `bot.heldItem` — the stack in the selected hotbar slot.
    pub fn held_item(&self) -> Option<Item> {
        held_item(&self.client, self.quick_bar_slot())
    }

    /// `bot.quickBarSlot` — the selected hotbar index, `0..=8`.
    pub fn quick_bar_slot(&self) -> u8 {
        if let Some(sel) = self.windows(|t| t.selected_hotbar).flatten() {
            return sel;
        }
        self.client
            .get_component::<mineflayer_engine::entity::inventory::Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    }

    /// `bot.setQuickBarSlot(slot)` — select a hotbar slot (`0..=8`),
    /// sending `held_item_slot` to the server (the engine's
    /// `set_selected_hotbar_slot` does both) and emitting
    /// [`BotEvent::HeldItemChanged`] like upstream's `setQuickBarSlot`.
    pub async fn set_quick_bar_slot(&self, slot: u8) -> Result<()> {
        if slot > 8 {
            return Err(Error::InvalidArgument(format!(
                "quick_bar_slot must be 0..=8, got {slot}"
            )));
        }
        if self.quick_bar_slot() == slot {
            return Ok(());
        }
        self.client.set_selected_hotbar_slot(slot);
        self.windows(|t| t.selected_hotbar = Some(slot));
        self.update_held_item();
        Ok(())
    }

    /// `bot.usingHeldItem`.
    pub fn using_held_item(&self) -> bool {
        self.windows(|t| t.using_held_item).unwrap_or_default()
    }

    /// `bot.getEquipmentDestSlot(destination)` — the absolute
    /// player-window slot for an equipment destination.
    pub fn equipment_dest_slot(&self, dest: EquipDest) -> u16 {
        dest_fixed_slot(dest)
            .unwrap_or_else(|| (QUICK_BAR_START + usize::from(self.quick_bar_slot())) as u16)
    }

    /// `bot.updateHeldItem()` — re-emit [`BotEvent::HeldItemChanged`].
    /// Real changes are also emitted packet-side; upstream calls this
    /// after click choreography.
    pub fn update_held_item(&self) {
        self.emit_event(BotEvent::HeldItemChanged(self.held_item()));
    }

    /// `bot.simpleClick.leftMouse(slot)` — one left click (mode 0).
    pub async fn simple_click_left(&self, slot: u16) -> Result<()> {
        self.click_window(i32::from(slot), 0, 0).await
    }

    /// `bot.simpleClick.rightMouse(slot)` — one right click (mode 0).
    pub async fn simple_click_right(&self, slot: u16) -> Result<()> {
        self.click_window(i32::from(slot), 1, 0).await
    }

    /// `bot.clickWindow(slot, mouseButton, mode)` — perform one
    /// container click on the open window (or the player inventory).
    ///
    /// Modes are upstream's: 0 normal click, 1 shift-click, 2 hotbar
    /// swap (`mouse_button` = hotbar index 0-8, or 40 for the off-hand
    /// key), 3 middle/clone click (`mouse_button` unused by the wire
    /// type), 4 drop (`mouse_button` 0 = single, 1 = stack). Modes 5
    /// (quick-craft drag) and 6 (pickup-all) error — upstream asserts
    /// `mode <= 6` but nothing in this port emits those gestures.
    ///
    /// `slot` may be `-999` to click outside (drop the cursor stack;
    /// `mouse_button` 0 = whole stack, 1 = single).
    ///
    /// Like upstream's `waitForWindowUpdate`, clicks on the crafting
    /// region (slots 0..=4 of the player window, 0..=9 of a crafting
    /// table) also wait for the server's crafting-result resync before
    /// returning — the packet that crafting needs.
    pub async fn click_window(&self, slot: i32, mouse_button: u8, mode: u8) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        click_window_on(&window, slot, mouse_button, mode, self.subscribe()).await
    }

    /// [`Bot::click_window`] with a typed the engine [`ClickOperation`] in
    /// place of upstream's `(slot, mouseButton, mode)` integer codes. The
    /// same crafting-result wait applies.
    pub async fn click_window_operation(&self, operation: ClickOperation) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        click_operation_on(&window, operation, self.subscribe()).await;
        Ok(())
    }

    /// `bot.putAway(slot)` — put one slot's stack back into the window's
    /// player-inventory region: a shift-click when there's room
    /// (matching not-full stack or empty slot) and the slot isn't a
    /// crafting/trade result; otherwise pick it up and drop it into the
    /// region manually. Resolves once the slot's `WindowUpdateSlot`
    /// arrives, like upstream's `once(window, 'updateSlot:${slot}')`.
    pub async fn put_away(&self, slot: u16) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        let mut rx = self.subscribe();
        let window_id = window.id();

        let item = window.slot(i32::from(slot));
        let player = window.player_range();
        // result slots (upstream `slot === window.craftingResultSlot`
        // plus the villager trade output) must not be put away via
        // shift-click — that's a craft/trade action, not a store
        let is_result_slot = (window.id() == 0 && slot == 0)
            || (window.menu_kind() == Some(MenuKind::Crafting) && slot == 0)
            || (window.menu_kind() == Some(MenuKind::Merchant) && slot == 2);
        let room = item.as_ref().is_some_and(|item| {
            let slots = window.slots().unwrap_or_default();
            find_item_in(
                &slots,
                player.clone(),
                item.kind(),
                item.metadata(),
                true,
                Some(item.component_patch()),
            )
            .is_some()
                || first_empty_in(&slots, player).is_some()
        });
        if room && !is_result_slot {
            self.click_window(i32::from(slot), 0, 1).await?;
        } else {
            self.click_window(i32::from(slot), 0, 0).await?;
            let player = window.player_range();
            put_selected(&window, player.start as i32..player.end as i32, None).await?;
        }
        // upstream awaits the slot's own updateSlot event
        wait_for_slot_update(&mut rx, window_id, slot).await;
        Ok(())
    }

    /// `bot._syncWindow(window)` — force the server to resend the
    /// authoritative window contents. 1.17.1+ servers only answer a
    /// click when their record of the client is stale, so upstream
    /// sends a deliberately impossible `window_click` (stateId `-1`,
    /// slot `-999`, `mouseButton 2`, `mode 5` — the end of a drag that
    /// never started); the rejection comes back as a full
    /// `container_set_content`, which heals any unconfirmed click
    /// predictions before `closeWindow` bakes them into the inventory
    /// view. Waits for that resync — 5 s cap like the other window
    /// waits so a dropped reply can't hang a craft. Upstream gates on
    /// `stateIdUsed`; mc26 always uses it.
    #[doc(hidden)]
    pub async fn sync_window(&self, window: &Window) {
        let mut rx = self.subscribe();
        let window_id = window.id();
        let registries = self.registry();
        let carried = window
            .selected_item()
            .map(|i| i.stack)
            .unwrap_or(ItemStack::Empty);
        self.client.write_packet(ServerboundContainerClick {
            container_id: window_id,
            // upstream's stateId -1 — u32::MAX is the same VarInt on the
            // wire and just as impossible for the server to have seen
            state_id: u32::MAX,
            slot_num: -999,
            button_num: 2, // QuickCraft: left-drag end
            click_type: ClickType::QuickCraft,
            changed_slots: Default::default(),
            carried_item: HashedStack::from_item_stack(&carried, &registries),
        });
        // every exit — ack, End, closed, timeout — ends the wait
        let _ = await_event(
            &mut rx,
            Duration::from_secs(5),
            |e| matches!(e, BotEvent::SetWindowItems { window_id: id } if *id == window_id),
        )
        .await;
    }

    /// `bot.closeWindow(window)` — send `container_close`, sync the
    /// player-inventory region back like upstream's `copyInventory`, and
    /// emit [`BotEvent::WindowClose`]. Idempotent; the server ack is
    /// deduplicated.
    pub fn close_window(&self, window: &Window) {
        window.close();
    }

    /// `bot.tossStack(item)` — put `item` on the cursor, drop it outside,
    /// then close the open window (upstream closes `currentWindow ||
    /// inventory` afterwards).
    pub async fn toss_stack(&self, item: &Item) -> Result<()> {
        self.click_window(i32::from(item.slot), 0, 0).await?;
        self.click_window(-999, 0, 0).await?;
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        self.close_window(&window);
        Ok(())
    }

    /// `bot.toss(itemType, metadata, count)` — drop `count` items of
    /// `kind` from the player-inventory region to the ground; `None` =
    /// 1 item (upstream `count ?? 1`).
    pub async fn toss(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
        nbt: Option<DataComponentPatch>,
    ) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        let player = window.player_range();
        self.transfer(TransferOpts {
            window: Some(window),
            kind,
            metadata,
            nbt,
            count,
            source_start: player.start as i32,
            source_end: Some(player.end as i32),
            dest_start: -999,
            dest_end: None,
        })
        .await
    }

    /// `bot.moveSlotItem(source, dest)` — pick up `source` and place it
    /// at `dest`; if the swap leaves the stack on the cursor, click
    /// `source` back like upstream.
    pub async fn move_slot_item(&self, source_slot: i32, dest_slot: i32) -> Result<()> {
        self.click_window(source_slot, 0, 0).await?;
        self.click_window(dest_slot, 0, 0).await?;
        self.update_held_item();
        // if we're holding an item, put it back where the source item was
        if self.inventory().selected_item().is_some() {
            self.click_window(source_slot, 0, 0).await?;
        }
        Ok(())
    }

    /// `bot.equip(item, destination)` — move `item` (an [`Item`] read
    /// from [`Bot::inventory`]) into an equipment destination.
    ///
    /// For `Hand`: if the item is already on the hotbar, just switch the
    /// quickbar selection; otherwise move it to the first empty hotbar
    /// slot — or to the rotating `nextQuickBarSlot` when the hotbar is
    /// full — then select it.
    pub async fn equip(&self, item: &Item, dest: EquipDest) -> Result<()> {
        // upstream: `item == null || item.slot === -1` → invalid
        if item.stack.is_empty() || item.slot == -1 {
            return Err(Error::InvalidArgument(
                "Invalid item object in equip".into(),
            ));
        }
        let source_slot = i32::from(item.slot);
        let mut dest_slot = i32::from(self.equipment_dest_slot(dest));
        if source_slot == dest_slot {
            return Ok(());
        }
        if dest != EquipDest::Hand {
            return self.move_slot_item(source_slot, dest_slot).await;
        }

        const QUICK_BAR_END: i32 = (QUICK_BAR_START + 9) as i32;
        let in_quickbar = |slot: i32| (QUICK_BAR_START as i32..QUICK_BAR_END).contains(&slot);
        if in_quickbar(dest_slot) && in_quickbar(source_slot) {
            // all we have to do is change the quick bar selection
            return self
                .set_quick_bar_slot((source_slot - QUICK_BAR_START as i32) as u8)
                .await;
        }

        // find an empty slot on the quick bar to put the source item in
        let slots = self.inventory().slots().unwrap_or_default();
        dest_slot = match first_empty_in(&slots, QUICK_BAR_START..QUICK_BAR_START + 9) {
            Some(i) => i as i32,
            None => {
                // LRU cache for the quick bar items
                let next = self
                    .windows(|t| {
                        let next = t.next_quick_bar_slot;
                        t.next_quick_bar_slot = (next + 1) % 9;
                        next
                    })
                    .unwrap_or_default();
                QUICK_BAR_START as i32 + i32::from(next)
            }
        };
        self.set_quick_bar_slot((dest_slot - QUICK_BAR_START as i32) as u8)
            .await?;
        self.move_slot_item(source_slot, dest_slot).await
    }

    /// `bot.equip` by kind — equip the first matching stack in the
    /// player-inventory region (upstream `equip(itemId, dest)` form).
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// bot.equip_kind(ItemKind::IronPickaxe, EquipDest::Hand).await?;
    /// bot.equip_kind(ItemKind::IronHelmet, EquipDest::Head).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn equip_kind(&self, kind: ItemKind, dest: EquipDest) -> Result<()> {
        let item = self.find_inventory_item(kind).ok_or_else(|| {
            Error::InvalidArgument(format!(
                "Invalid item object in equip (item is null): {}",
                kind.to_str()
            ))
        })?;
        self.equip(&item, dest).await
    }

    /// `bot.findInventoryItem(type)` — first [`Item`] of `kind` in the
    /// player-inventory region of the inventory window.
    pub fn find_inventory_item(&self, kind: ItemKind) -> Option<Item> {
        let window = self.inventory();
        let range = window.player_range();
        window.find_item_range(range, kind, None, false, None)
    }

    /// `bot.unequip(destination)` — clear an equipment destination into
    /// the inventory. `Hand` switches to an empty quickbar slot when one
    /// exists (else moves the held stack into the inventory, or drops it
    /// if full — upstream `equipEmpty`). Other destinations require no
    /// open window (upstream `disrobe` asserts it) and route through
    /// [`Bot::put_away`].
    pub async fn unequip(&self, dest: EquipDest) -> Result<()> {
        match dest {
            EquipDest::Hand => self.equip_empty().await,
            _ => {
                if self.current_window().is_some() {
                    return Err(Error::InvalidState(
                        "can not unequip with open window".into(),
                    ));
                }
                self.put_away(self.equipment_dest_slot(dest)).await
            }
        }
    }

    /// Upstream `equipEmpty` — empty the hand.
    async fn equip_empty(&self) -> Result<()> {
        let inventory = self.inventory();
        // the quick-bar index below needs the real slot list; a missing
        // Inventory (disconnected/despawned) is `NotConnected`, not an
        // empty vector to index into
        let slots = inventory.slots().ok_or(Error::NotConnected)?;
        for i in 0..9usize {
            if slots[QUICK_BAR_START + i].is_empty() {
                return self.set_quick_bar_slot(i as u8).await;
            }
        }
        let Some(slot) = first_empty_in(&slots, 9..45) else {
            let held = self
                .held_item()
                .ok_or_else(|| Error::InvalidState("no held item to toss".into()))?;
            return self.toss_stack(&held).await;
        };
        let equip_slot = i32::from(self.equipment_dest_slot(EquipDest::Hand));
        self.click_window(equip_slot, 0, 0).await?;
        self.click_window(slot as i32, 0, 0).await?;
        if self.inventory().selected_item().is_some() {
            self.click_window(-999, 0, 0).await?;
        }
        Ok(())
    }

    /// `bot.activateItem(offHand)` — right-click air with the held item:
    /// send `use_item` for the main or off hand and set `using_held_item`
    /// (the packet is not sent for an empty hand — upstream returns
    /// early the same way).
    pub fn activate_item(&self, off_hand: bool) {
        let has_item = if off_hand {
            self.inventory()
                .slot(45)
                .is_some_and(|i| i.stack.is_present())
        } else {
            self.held_item().is_some()
        };
        if !has_item {
            return;
        }
        self.windows(|t| t.using_held_item = true);
        let seq = {
            let mut ecs = self.client.ecs.write();
            ecs.get_mut::<mineflayer_engine::interact::BlockStatePredictionHandler>(
                self.client.entity,
            )
            .map(|mut h| h.start_predicting())
            .unwrap_or_default()
        };
        let (y_rot, x_rot) = self
            .client
            .get_component::<LookDirection>()
            .map(|d| (d.y_rot(), d.x_rot()))
            .unwrap_or_default();
        self.client.write_packet(ServerboundUseItem {
            hand: if off_hand {
                mineflayer_engine::protocol::packets::game::s_interact::InteractionHand::OffHand
            } else {
                mineflayer_engine::protocol::packets::game::s_interact::InteractionHand::MainHand
            },
            seq,
            y_rot,
            x_rot,
        });
    }

    /// `bot.deactivateItem()` — release the held item (`player_action`
    /// status 5 / `ReleaseUseItem`) and clear `using_held_item`.
    pub fn deactivate_item(&self) {
        self.windows(|t| t.using_held_item = false);
        self.client.write_packet(ServerboundPlayerAction {
            action: s_player_action::Action::ReleaseUseItem,
            pos: BlockPos::new(0, 0, 0),
            // upstream sends face 0 (down) with sequence 0
            direction: Direction::Down,
            seq: 0,
        });
    }

    /// `bot.consume()` — start using the held food/potion and wait for
    /// the consumption finish signal (entity status 9, a cooldown
    /// packet, or the held stack changing kind/count).
    ///
    /// Errors `"Food is full"` for normal food at max hunger outside
    /// creative (upstream check), `"Consuming cancelled due to calling
    /// bot.consume() again"` when a second call supersedes this one, and
    /// [`Error::Timeout`] after 2.5 s (upstream `CONSUME_TIMEOUT`).
    pub async fn consume(&self) -> Result<()> {
        if self.game_mode() != Some(GameMode::Creative)
            && self
                .held_item()
                .is_some_and(|i| !ALWAYS_CONSUMABLES.contains(&i.kind()))
            && self.food() == 20
        {
            return Err(Error::InvalidState("Food is full".into()));
        }
        if self.held_item().is_none() {
            // upstream activateItem() silently returns on an empty hand
            // and the eat task then hangs; erroring is strictly better
            return Err(Error::InvalidState("Nothing held to consume".into()));
        }
        self.consume_with(false, Duration::from_millis(2500)).await
    }

    /// The use and the wait of [`Bot::consume`], with no food check, for
    /// an eat plugin: `offhand` uses the offhand item, and `timeout`
    /// bounds the wait. A newer consume or [`Bot::cancel_consume`] ends it
    /// with [`Error::Cancelled`].
    pub async fn consume_with(&self, offhand: bool, timeout: Duration) -> Result<()> {
        // start the eat before activate_item so a finish packet that
        // lands between the use_item write and the wait can't be missed
        // (upstream creates the eat task before activating)
        let (op, on_finish) = self.start_eat().ok_or(Error::NotConnected)?;
        self.activate_item(offhand);
        match self.wait_eat(&op, on_finish, timeout).await {
            EatEnd::Finished => Ok(()),
            EatEnd::Superseded => Err(Error::Cancelled(
                "Consuming cancelled due to calling bot.consume() again".into(),
            )),
            EatEnd::Timeout => Err(Error::Timeout),
            EatEnd::Disconnected => Err(Error::NotConnected),
        }
    }

    /// End the running consume, as a newer consume would.
    pub fn cancel_consume(&self) {
        self.windows(|t| t.eat.cancel());
    }

    /// `bot.openBlock(block)` — look at the block (upstream
    /// `activateBlock`'s `lookAt`), right-click it, and wait for the
    /// server's `open_screen` + slot data; resolves with the opened
    /// [`Window`]. The upstream `direction`/`cursorPos` hit-result
    /// parameters have no engine equivalent (the engine fabricates the
    /// block-face hit from `force_block`).
    pub async fn open_block(&self, block: &Block) -> Result<Window> {
        self.open_at(block.pos).await
    }

    /// Look at the block at `pos`, right-click it, and wait for the window.
    async fn open_at(&self, pos: BlockPos) -> Result<Window> {
        let mut rx = self.subscribe();
        self.look_at(pos.center(), false).await?;
        self.client.block_interact(pos);
        self.await_window_open(&mut rx).await
    }

    /// `bot.openEntity(entity)` — look at the entity, right-click a
    /// container-bearing entity (chest minecart, llama, …), and wait
    /// for the window.
    pub async fn open_entity(&self, entity: &mineflayer_engine::EntityRef) -> Result<Window> {
        let mut rx = self.subscribe();
        // upstream activateEntity's lookAt aims at the entity's
        // position (feet); half-height approximates the body's center
        if let Some(pos) = entity
            .get_component::<mineflayer_engine::entity::Position>()
            .map(|p| **p)
        {
            let mid = entity
                .get_component::<mineflayer_engine::entity::dimensions::EntityDimensions>()
                .map(|d| f64::from(d.height) / 2.)
                .unwrap_or(0.5);
            self.look_at(pos + mineflayer_engine::Vec3::new(0., mid, 0.), false)
                .await?;
        }
        entity.interact();
        self.await_window_open(&mut rx).await
    }

    /// `bot.openContainer`/`bot.openChest`/`bot.openDispenser` — open a
    /// block or entity and verify the resulting window is a container
    /// kind (upstream `matchWindowType`). On a menu-kind mismatch this
    /// closes the window before erroring — upstream throws and leaves
    /// it open; closing is the safer default and is noted in
    /// `docs/api-parity.md`.
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// let chest = bot
    ///     .find_block(FindBlocksOpts::new(BlockKind::Chest).max_distance(6.0))
    ///     .ok_or_else(|| Error::NotFound("no chest nearby".into()))?;
    /// let window = bot.open_container(&chest).await?;
    /// window.deposit(ItemKind::Cobblestone, None, Some(16), None).await?;
    /// window.close();
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open_container(&self, target: impl Into<ContainerTarget>) -> Result<Window> {
        let window = match target.into() {
            ContainerTarget::Block { pos, kind } => {
                if !is_container_block(kind) {
                    return Err(Error::InvalidArgument(
                        "containerToOpen is neither a block nor an entity".into(),
                    ));
                }
                self.open_at(pos).await?
            }
            ContainerTarget::Entity(entity) => self.open_entity(&entity).await?,
        };
        match window.menu_kind() {
            Some(kind) if is_container_menu(kind) => Ok(window),
            other => {
                window.close();
                Err(Error::InvalidArgument(format!(
                    "Non-container window used as a container: {other:?}"
                )))
            }
        }
    }

    /// `bot.openChest` — upstream aliases all three openers to
    /// `openContainer`; kept for API-name parity.
    pub async fn open_chest(&self, target: impl Into<ContainerTarget>) -> Result<Window> {
        self.open_container(target).await
    }

    /// `bot.openDispenser` — same as [`Bot::open_chest`] upstream.
    pub async fn open_dispenser(&self, target: impl Into<ContainerTarget>) -> Result<Window> {
        self.open_container(target).await
    }

    /// Wait for the next `WindowOpen` — upstream's
    /// `once(bot, 'windowOpen')` after `activateBlock`/`activateEntity`.
    async fn await_window_open(&self, rx: &mut broadcast::Receiver<BotEvent>) -> Result<Window> {
        Ok(await_map(rx, Duration::from_secs(5), |e| match e {
            BotEvent::WindowOpen(w) => Some(w.clone()),
            _ => None,
        })
        .await?)
    }
}

/// Send one click operation on `window` and — like upstream's
/// `waitForWindowUpdate` — wait for the server's crafting-result resync
/// when the click lands in a crafting region (player inventory slots
/// 0..=4, crafting-table slots 0..=9). The merchant-window branches of
/// upstream are not ported; the typed villager window owns trades.
pub(crate) async fn click_window_on(
    window: &Window,
    slot: i32,
    mouse_button: u8,
    mode: u8,
    events: broadcast::Receiver<BotEvent>,
) -> Result<()> {
    let op = match slot {
        -999 => match mouse_button {
            0 => ClickOperation::Pickup(PickupClick::LeftOutside),
            1 => ClickOperation::Pickup(PickupClick::RightOutside),
            b => return Err(Error::InvalidArgument(format!("invalid mouse button {b}"))),
        },
        s if s < 0 => return Err(Error::InvalidArgument(format!("invalid slot {s}"))),
        s => click_operation(s as u16, mouse_button, mode)?,
    };
    click_operation_on(window, op, events).await;
    Ok(())
}

/// Whether a click on `slot` lands on the crafting region: slots 0..=4 of
/// the player window or 0..=9 of a crafting table. An outside click
/// (`None`, slot -999 on the wire) never waits.
fn waits_for_craft_result(window_id: i32, menu_kind: Option<MenuKind>, slot: Option<u16>) -> bool {
    slot.is_some_and(|slot| {
        (window_id == 0 && slot <= 4) || (menu_kind == Some(MenuKind::Crafting) && slot <= 9)
    })
}

/// Send one click, then — like upstream's `waitForWindowUpdate` — wait
/// for the crafting-result resync when the click lands on the crafting
/// region (slots 0..=4 of the player window, 0..=9 of a crafting table).
async fn click_operation_on(
    window: &Window,
    op: ClickOperation,
    mut events: broadcast::Receiver<BotEvent>,
) {
    let waits = waits_for_craft_result(window.id(), window.menu_kind(), op.slot_num());
    window.click(op);
    if waits {
        wait_for_slot_update(&mut events, window.id(), 0).await;
    }
}

/// Await the [`BotEvent::WindowUpdateSlot`] for `window_id`/`slot` —
/// upstream's `once(window, 'updateSlot:${slot}')`, with a 5 s
/// (`WINDOW_TIMEOUT`) cap so a dropped packet can't hang the caller.
async fn wait_for_slot_update(rx: &mut broadcast::Receiver<BotEvent>, window_id: i32, slot: u16) {
    // one slot, one wait — callers that need several slots share a single
    // loop (`window_types::wait_for_slot_updates`); sequential per-slot
    // waits would eat each other's events
    let _ = await_event(rx, Duration::from_secs(5), |e| {
        matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: wid,
                slot: s,
                ..
            } if *wid == window_id && *s == slot
        )
    })
    .await;
}

#[cfg(test)]
mod tests;
