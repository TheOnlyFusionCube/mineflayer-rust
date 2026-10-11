//! Inventory reads, item use, equipment, and window clicks — ports of
//! `inventory.js`, `simple_inventory.js`, and the openers of `chest.js`.
//!
//! Window state lives in [`crate::windows`]; this module is the `Bot`
//! method surface over it. Click choreography (equip, transfer, putAway,
//! toss) follows upstream `inventory.js`/`simple_inventory.js` closely;
//! where upstream clicks through `clickWindow` we send the equivalent
//! azalea [`ClickOperation`] on the same window.

use std::{ops::Range, time::Duration};

use azalea::{
    BlockPos,
    core::{direction::Direction, game_type::GameMode},
    entity::LookDirection,
    inventory::{
        DataComponentPatch,
        operations::{ClickOperation, PickupClick, QuickMoveClick, SwapClick, ThrowClick},
    },
    protocol::packets::game::{
        s_player_action::{self, ServerboundPlayerAction},
        s_use_item::ServerboundUseItem,
    },
    registry::builtin::{ItemKind, MenuKind},
};
use tokio::sync::broadcast;

use crate::{
    Bot, BotEvent, Error, Result,
    windows::{Item, Window, find_item_in, first_empty_in, held_item},
    world::Block,
};

/// `bot.QUICK_BAR_START` — the absolute player-window slot where the
/// 9-slot hotbar begins (upstream 36).
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
    /// A container block: position + name for upstream's allowed-list
    /// pre-check (chest.js throws before opening other blocks).
    Block { pos: BlockPos, name: String },
    /// An entity reference.
    Entity(azalea::EntityRef),
}

impl From<&Block> for ContainerTarget {
    fn from(block: &Block) -> Self {
        ContainerTarget::Block {
            pos: block.pos,
            name: block.name().replace("minecraft:", ""),
        }
    }
}

impl From<Block> for ContainerTarget {
    fn from(block: Block) -> Self {
        (&block).into()
    }
}

impl From<azalea::EntityRef> for ContainerTarget {
    fn from(entity: azalea::EntityRef) -> Self {
        ContainerTarget::Entity(entity)
    }
}

impl From<&azalea::EntityRef> for ContainerTarget {
    fn from(entity: &azalea::EntityRef) -> Self {
        ContainerTarget::Entity(entity.clone())
    }
}

/// Upstream chest.js `allowedWindowTypes` as block names — the pre-open
/// check for [`ContainerTarget::Block`].
fn is_container_block_name(name: &str) -> bool {
    matches!(
        name,
        "generic"
            | "chest"
            | "trapped_chest"
            | "ender_chest"
            | "dispenser"
            | "dropper"
            | "hopper"
            | "barrel"
            | "container"
            | "shulker_box"
    ) || name.ends_with("_shulker_box")
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

/// `bot.transfer(options)` parameters. Ranges are `[start, end)` in
/// absolute window-slot indices (the same convention upstream's
/// `sourceStart`/`sourceEnd`/`destStart`/`destEnd` use); `dest_start ==
/// -999` drops items on the ground.
#[derive(Clone, Debug)]
pub struct TransferOpts {
    /// Window to act on — `None` means the open window, else the player
    /// inventory (upstream `options.window || currentWindow ||
    /// inventory`).
    pub window: Option<Window>,
    /// Only stacks of this kind move (upstream `itemType`).
    pub kind: ItemKind,
    /// Required `damage` component (upstream `metadata`); `None` = any.
    pub metadata: Option<i32>,
    /// Required component patch (upstream `nbt`); `None` = any.
    pub nbt: Option<DataComponentPatch>,
    /// How many items to move; `None` = 1 (upstream `count ?? 1`).
    pub count: Option<i32>,
    /// Source range start (absolute window slot).
    pub source_start: i32,
    /// Source range end, exclusive — defaults to `source_start + 1`.
    pub source_end: Option<i32>,
    /// Destination range start; `-999` drops the items.
    pub dest_start: i32,
    /// Destination range end, exclusive — defaults to `dest_start + 1`.
    pub dest_end: Option<i32>,
}

/// `(mouseButton, mode)` → the azalea click operation, upstream's
/// `clickWindow` mode dispatch. `slot` is the absolute window slot
/// (`-999` is handled in [`Bot::click_window`], which sends the outside
/// clicks).
pub(crate) fn click_operation(slot: u16, mouse_button: u8, mode: u8) -> Result<ClickOperation> {
    Ok(match mode {
        0 => match mouse_button {
            0 => PickupClick::Left { slot: Some(slot) }.into(),
            1 => PickupClick::Right { slot: Some(slot) }.into(),
            b => return Err(Error::Operation(format!("invalid mouse button {b}"))),
        },
        1 => match mouse_button {
            0 => QuickMoveClick::Left { slot }.into(),
            1 => QuickMoveClick::Right { slot }.into(),
            b => return Err(Error::Operation(format!("invalid mouse button {b}"))),
        },
        2 => SwapClick {
            source_slot: slot,
            target_slot: mouse_button,
        }
        .into(),
        3 => ThrowClick::All { slot }.into(),
        4 => match mouse_button {
            0 => ThrowClick::Single { slot }.into(),
            1 => ThrowClick::All { slot }.into(),
            b => return Err(Error::Operation(format!("invalid mouse button {b}"))),
        },
        // mode 5 (quick-craft drag) is a multi-packet gesture upstream
        // sends raw; nothing in the ported API emits it.
        5 | 6 => {
            return Err(Error::Operation(format!("unsupported mouse mode {mode}")));
        }
        m => return Err(Error::Operation(format!("invalid mode {m}"))),
    })
}

impl Bot {
    /// `bot.inventory` — the player's own inventory as a [`Window`] (id
    /// 0). Always available; a fresh handle per call (upstream keeps one
    /// object — slot reads are equivalent since both read live state).
    pub fn inventory(&self) -> Window {
        Window::player_inventory(self.client.clone(), &self.windows)
    }

    /// `bot.currentWindow` — the open non-inventory window, `None` when
    /// only the player inventory is open.
    pub fn current_window(&self) -> Option<Window> {
        let t = self.windows.lock().unwrap();
        t.current.as_ref().map(|shared| Window {
            shared: shared.clone(),
        })
    }

    /// `bot.heldItem` — the stack in the selected hotbar slot.
    pub fn held_item(&self) -> Option<Item> {
        held_item(&self.client, self.quick_bar_slot())
    }

    /// `bot.quickBarSlot` — the selected hotbar index, `0..=8`.
    pub fn quick_bar_slot(&self) -> u8 {
        if let Some(sel) = self.windows.lock().unwrap().selected_hotbar {
            return sel;
        }
        self.client
            .get_component::<azalea::entity::inventory::Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    }

    /// `bot.setQuickBarSlot(slot)` — select a hotbar slot (`0..=8`),
    /// sending `held_item_slot` to the server (azalea's
    /// `set_selected_hotbar_slot` does both) and emitting
    /// [`BotEvent::HeldItemChanged`] like upstream's `setQuickBarSlot`.
    pub async fn set_quick_bar_slot(&self, slot: u8) -> Result<()> {
        if slot > 8 {
            return Err(Error::Operation(format!(
                "quick_bar_slot must be 0..=8, got {slot}"
            )));
        }
        if self.quick_bar_slot() == slot {
            return Ok(());
        }
        self.client.set_selected_hotbar_slot(slot);
        self.windows.lock().unwrap().selected_hotbar = Some(slot);
        self.update_held_item().await;
        Ok(())
    }

    /// `bot.usingHeldItem`.
    pub fn using_held_item(&self) -> bool {
        self.windows.lock().unwrap().using_held_item
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
    pub async fn update_held_item(&self) {
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
    /// key), 3 middle/clone (upstream's modeMap maps 3 to drop-all),
    /// 4 drop (`mouse_button` 0 = single, 1 = stack). Modes 5
    /// (quick-craft drag) and 6 error — upstream asserts `mode <= 6` but
    /// nothing in this port emits those gestures.
    ///
    /// `slot` may be `-999` to click outside (drop the cursor stack;
    /// `mouse_button` 0 = whole stack, 1 = single).
    ///
    /// Like upstream's `waitForWindowUpdate`, clicks on the crafting
    /// region (slots 0..=4 of the player window, 0..=9 of a crafting
    /// table) also wait for the server's crafting-result resync before
    /// returning — the packet that matters for U10 crafting.
    pub async fn click_window(&self, slot: i32, mouse_button: u8, mode: u8) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        click_window_on(&window, slot, mouse_button, mode, self.events()).await
    }

    /// `bot.putSelectedItemRange(start, end, window, reservedSlot)` —
    /// put whatever is on the cursor into `[start, end)`: merge onto
    /// matching not-full stacks first, then the first empty slot, else
    /// click `reserved_slot` and toss the leftover outside (upstream's
    /// nested helper, with the `slot` parameter).
    pub async fn put_selected_item_range(
        &self,
        start: i32,
        end: i32,
        window: &Window,
        reserved_slot: Option<i32>,
    ) -> Result<()> {
        put_selected(window, start..end, reserved_slot).await
    }

    /// `bot.putAway(slot)` — put one slot's stack back into the window's
    /// player-inventory region: a shift-click when there's room
    /// (matching not-full stack or empty slot) and the slot isn't a
    /// crafting/trade result; otherwise pick it up and drop it into the
    /// region manually. Resolves once the slot's `WindowUpdateSlot`
    /// arrives, like upstream's `once(window, 'updateSlot:${slot}')`.
    pub async fn put_away(&self, slot: u16) -> Result<()> {
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        let mut rx = self.events();
        let window_id = window.id();

        let item = window.slot(i32::from(slot));
        let player = window.player_range();
        let is_result_slot = (window.id() == 0 && slot == 0)
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

    /// `bot.closeWindow(window)` — send `container_close`, sync the
    /// player-inventory region back like upstream's `copyInventory`, and
    /// emit [`BotEvent::WindowClose`]. Idempotent; the server ack is
    /// deduplicated.
    pub async fn close_window(&self, window: &Window) {
        window.close();
    }

    /// `bot.tossStack(item)` — put `item` on the cursor, drop it outside,
    /// then close the open window (upstream closes `currentWindow ||
    /// inventory` afterwards).
    pub async fn toss_stack(&self, item: &Item) -> Result<()> {
        self.click_window(i32::from(item.slot), 0, 0).await?;
        self.click_window(-999, 0, 0).await?;
        let window = self.current_window().unwrap_or_else(|| self.inventory());
        self.close_window(&window).await;
        Ok(())
    }

    /// `bot.toss(itemType, metadata, count)` — drop `count` items of
    /// `kind` from the player-inventory region to the ground; `None`
    /// moves every matching item (upstream `count === null`).
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

    /// `bot.transfer(options)` — move items between slot ranges with the
    /// upstream click choreography (see [`transfer`]).
    pub async fn transfer(&self, options: TransferOpts) -> Result<()> {
        let window = options
            .window
            .clone()
            .or_else(|| self.current_window())
            .unwrap_or_else(|| self.inventory());
        transfer(TransferOpts {
            window: Some(window),
            ..options
        })
        .await
    }

    /// `bot.moveSlotItem(source, dest)` — pick up `source` and place it
    /// at `dest`; if the swap leaves the stack on the cursor, click
    /// `source` back like upstream.
    pub async fn move_slot_item(&self, source_slot: i32, dest_slot: i32) -> Result<()> {
        self.click_window(source_slot, 0, 0).await?;
        self.click_window(dest_slot, 0, 0).await?;
        self.update_held_item().await;
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
            return Err(Error::Operation("Invalid item object in equip".into()));
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
                let mut t = self.windows.lock().unwrap();
                let slot = QUICK_BAR_START as i32 + i32::from(t.next_quick_bar_slot);
                t.next_quick_bar_slot = (t.next_quick_bar_slot + 1) % 9;
                slot
            }
        };
        self.set_quick_bar_slot((dest_slot - QUICK_BAR_START as i32) as u8)
            .await?;
        self.move_slot_item(source_slot, dest_slot).await
    }

    /// `bot.equip` by kind — equip the first matching stack in the
    /// player-inventory region (upstream `equip(itemId, dest)` form).
    pub async fn equip_kind(&self, kind: ItemKind, dest: EquipDest) -> Result<()> {
        let item = self.find_inventory_item(kind).ok_or_else(|| {
            Error::Operation(format!(
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
                    return Err(Error::Operation("can not unequip with open window".into()));
                }
                self.put_away(self.equipment_dest_slot(dest)).await
            }
        }
    }

    /// Upstream `equipEmpty` — empty the hand.
    async fn equip_empty(&self) -> Result<()> {
        let inventory = self.inventory();
        let slots = inventory.slots().unwrap_or_default();
        for i in 0..9usize {
            if slots[QUICK_BAR_START + i].is_empty() {
                return self.set_quick_bar_slot(i as u8).await;
            }
        }
        let Some(slot) = first_empty_in(&slots, 9..45) else {
            let held = self
                .held_item()
                .ok_or_else(|| Error::Operation("no held item to toss".into()))?;
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
    pub async fn activate_item(&self, off_hand: bool) {
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
        self.windows.lock().unwrap().using_held_item = true;
        let seq = {
            let mut ecs = self.client.ecs.write();
            ecs.get_mut::<azalea::interact::BlockStatePredictionHandler>(self.client.entity)
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
                azalea::protocol::packets::game::s_interact::InteractionHand::OffHand
            } else {
                azalea::protocol::packets::game::s_interact::InteractionHand::MainHand
            },
            seq,
            y_rot,
            x_rot,
        });
    }

    /// `bot.deactivateItem()` — release the held item (`player_action`
    /// status 5 / `ReleaseUseItem`) and clear `using_held_item`.
    pub async fn deactivate_item(&self) {
        self.windows.lock().unwrap().using_held_item = false;
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
            return Err(Error::Operation("Food is full".into()));
        }
        if self.held_item().is_none() {
            // upstream activateItem() silently returns on an empty hand
            // and the eat task then hangs; erroring is strictly better
            return Err(Error::Operation("Nothing held to consume".into()));
        }
        let (generation, notify) = {
            let mut t = self.windows.lock().unwrap();
            t.eat_generation += 1;
            (t.eat_generation, t.eat_notify.clone())
        };
        self.activate_item(false).await;
        if tokio::time::timeout(Duration::from_millis(2500), notify.notified())
            .await
            .is_err()
        {
            return Err(Error::Timeout);
        }
        if self.windows.lock().unwrap().eat_generation == generation {
            Ok(())
        } else {
            Err(Error::Operation(
                "Consuming cancelled due to calling bot.consume() again".into(),
            ))
        }
    }

    /// `bot.openBlock(block)` — right-click a container block and wait
    /// for the server's `open_screen` + slot data; resolves with the
    /// opened [`Window`]. The upstream `direction`/`cursorPos` hit-result
    /// parameters have no azalea equivalent (azalea fabricates the
    /// block-face hit from `force_block`).
    pub async fn open_block(&self, block: &Block) -> Result<Window> {
        let mut rx = self.events();
        self.client.block_interact(block.pos);
        self.await_window_open(&mut rx).await
    }

    /// `bot.openEntity(entity)` — right-click a container-bearing entity
    /// (chest minecart, llama, …) and wait for the window.
    pub async fn open_entity(&self, entity: &azalea::EntityRef) -> Result<Window> {
        let mut rx = self.events();
        entity.interact();
        self.await_window_open(&mut rx).await
    }

    /// `bot.openContainer`/`bot.openChest`/`bot.openDispenser` — open a
    /// block or entity and verify the resulting window is a container
    /// kind (upstream `matchWindowType`); errors and closes otherwise.
    pub async fn open_container(&self, target: impl Into<ContainerTarget>) -> Result<Window> {
        let window = match target.into() {
            ContainerTarget::Block { pos, name } => {
                if !is_container_block_name(&name) {
                    return Err(Error::Operation(
                        "containerToOpen is neither a block nor an entity".into(),
                    ));
                }
                let mut rx = self.events();
                self.client.block_interact(pos);
                self.await_window_open(&mut rx).await?
            }
            ContainerTarget::Entity(entity) => {
                let mut rx = self.events();
                entity.interact();
                self.await_window_open(&mut rx).await?
            }
        };
        match window.menu_kind() {
            Some(kind) if is_container_menu(kind) => Ok(window),
            other => {
                window.close();
                Err(Error::Operation(format!(
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
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(BotEvent::WindowOpen(w)) => return Ok(w),
                    Ok(BotEvent::End(_)) => return Err(Error::NotConnected),
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::NotConnected);
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .map_err(|_| Error::Timeout)?
    }
}

/// Send one click operation on `window` and — like upstream's
/// `waitForWindowUpdate` — wait for the server's crafting-result resync
/// when the click lands in a crafting region (player inventory slots
/// 0..=4, crafting-table slots 0..=9). The merchant-window branches of
/// upstream are U9 territory and intentionally not ported.
pub(crate) async fn click_window_on(
    window: &Window,
    slot: i32,
    mouse_button: u8,
    mode: u8,
    mut events: broadcast::Receiver<BotEvent>,
) -> Result<()> {
    let waits_for_craft_result = (window.id() == 0 && (0..=4).contains(&slot))
        || (window.menu_kind() == Some(MenuKind::Crafting) && (0..=9).contains(&slot));
    let op = match slot {
        -999 => match mouse_button {
            0 => ClickOperation::Pickup(PickupClick::LeftOutside),
            1 => ClickOperation::Pickup(PickupClick::RightOutside),
            b => return Err(Error::Operation(format!("invalid mouse button {b}"))),
        },
        s if s < 0 => return Err(Error::Operation(format!("invalid slot {s}"))),
        s => click_operation(s as u16, mouse_button, mode)?,
    };
    window.click(op);
    if waits_for_craft_result {
        wait_for_slot_update(&mut events, window.id(), 0).await;
    }
    Ok(())
}

/// Await the [`BotEvent::WindowUpdateSlot`] for `window_id`/`slot` —
/// upstream's `once(window, 'updateSlot:${slot}')`, with a 5 s
/// (`WINDOW_TIMEOUT`) cap so a dropped packet can't hang the caller.
async fn wait_for_slot_update(rx: &mut broadcast::Receiver<BotEvent>, window_id: i32, slot: u16) {
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(BotEvent::WindowUpdateSlot {
                    window_id: wid,
                    slot: s,
                    ..
                }) if wid == window_id && s == slot => return,
                Ok(BotEvent::End(_)) => return,
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), wait).await;
}

/// `putSelectedItemRange` (upstream): while the cursor holds a stack,
/// merge it onto a not-full matching stack in `[start, end)`, else the
/// first empty slot, else (reserved `slot`) click it, else toss the
/// leftover outside.
async fn put_selected(window: &Window, range: Range<i32>, slot: Option<i32>) -> Result<()> {
    while let Some(carried) = window.selected_item() {
        let slots = window.slots().unwrap_or_default();
        let range = range.start.max(0) as usize..range.end.max(0) as usize;
        let merge = find_item_in(
            &slots,
            range.clone(),
            carried.kind(),
            carried.metadata(),
            true,
            Some(carried.component_patch()),
        );
        if let Some(item) = merge
            && item.stack_size() != item.count()
        {
            window.click(PickupClick::Left {
                slot: Some(item.slot as u16),
            });
            continue;
        }
        match first_empty_in(&slots, range) {
            Some(empty) => {
                window.click(PickupClick::Left {
                    slot: Some(empty as u16),
                });
            }
            None => {
                if let Some(reserved) = slot {
                    window.click(PickupClick::Left {
                        slot: Some(reserved as u16),
                    });
                }
                toss_leftover(window);
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Upstream `tossLeftover` — click outside to drop the cursor stack.
fn toss_leftover(window: &Window) {
    if window.selected_item().is_some() {
        window.click(PickupClick::LeftOutside);
    }
}

/// `bot.transfer` (upstream inventory.js) — move `count` items of `kind`
/// from `[source_start, source_end)` to `[dest_start, dest_end)` using
/// pick-up/merge/place click choreography. `dest_start == -999` drops
/// them on the ground.
///
/// Unlike upstream (which clicks through `clickWindow` on the *current*
/// window), the operations go to `options.window` directly — the same
/// thing for every call site that matters, and the only correct thing
/// when a different window is passed.
pub(crate) async fn transfer(options: TransferOpts) -> Result<()> {
    let window = options
        .window
        .clone()
        .ok_or_else(|| Error::Operation("transfer requires a window".into()))?;
    let kind = options.kind;
    let metadata = options.metadata;
    let nbt = options.nbt;
    // upstream `count === null` means "move every matching item"; a
    // saturating sentinel behaves identically since the loop exits on
    // `count == 0` or by exhausting the source range.
    let mut count = options.count.unwrap_or(i32::MAX);
    let source = options.source_start..options.source_end.unwrap_or(options.source_start + 1);
    let dest_start = options.dest_start;
    let dest_end = options.dest_end.unwrap_or(dest_start + 1);
    let dest = dest_start..dest_end;
    let mut first_source_slot: Option<i32> = None;

    loop {
        if count == 0 {
            // put anything left on the cursor back into the source range
            put_selected(&window, source.clone(), first_source_slot).await?;
            return Ok(());
        }

        let held_matches = window.selected_item().is_some_and(|sel| {
            sel.kind() == kind
                && metadata.is_none_or(|m| sel.metadata() == Some(m))
                && nbt.as_ref().is_none_or(|p| sel.component_patch() == p)
        });
        if !held_matches {
            // we are not holding the item we need — click it
            let source_item = window
                .find_item_range(
                    source.start.max(0) as usize..source.end.max(0) as usize,
                    kind,
                    metadata,
                    false,
                    nbt.as_ref(),
                )
                .ok_or_else(|| {
                    Error::Operation(format!(
                        "Can't find {} in slots [{} - {}], (item id: {})",
                        kind.to_str(),
                        source.start,
                        source.end,
                        kind.to_str()
                    ))
                })?;
            if first_source_slot.is_none() {
                first_source_slot = Some(i32::from(source_item.slot));
            }
            window.click(PickupClick::Left {
                slot: Some(source_item.slot as u16),
            });
        }

        // clickDest()
        let Some(selected) = window.selected_item() else {
            return Err(Error::Operation(
                "transfer: cursor empty after click".into(),
            ));
        };
        let dest_slot: i32;
        let mut dest_slot_count = 0;
        if dest_start == -999 {
            dest_slot = -999;
        } else {
            let slots = window.slots().unwrap_or_default();
            let range = dest.start.max(0) as usize..dest.end.max(0) as usize;
            // a non-full same-item stack we can merge into
            let dest_item = find_item_in(
                &slots,
                range.clone(),
                selected.kind(),
                selected.metadata(),
                true,
                nbt.as_ref(),
            );
            match dest_item {
                Some(item) => {
                    dest_slot = i32::from(item.slot);
                    dest_slot_count = item.count();
                }
                None => match first_empty_in(&slots, range) {
                    Some(i) => dest_slot = i as i32,
                    None => {
                        return Err(Error::Operation("destination full".into()));
                    }
                },
            }
        }
        // move the maximum number of items a click can move
        let stack_size = selected.stack_size();
        let moved_items = (stack_size - dest_slot_count).min(selected.count());
        let target = dest_slot_count + count;
        let half = ((dest_slot_count + selected.count()).min(stack_size)) / 2;
        if moved_items <= count {
            // several at the same time (left click)
            window.click(if dest_slot == -999 {
                PickupClick::LeftOutside
            } else {
                PickupClick::Left {
                    slot: Some(dest_slot as u16),
                }
            });
            count -= moved_items;
        } else if dest_slot != -999 && half <= target && 3 + target - half < count {
            // A right click on a stack with an empty cursor takes the
            // larger half — cheaper than `count` right clicks.
            window.click(PickupClick::Left {
                slot: Some(dest_slot as u16),
            });
            if window.selected_item().is_some() {
                put_selected(&window, source.clone(), first_source_slot).await?;
            }
            window.click(PickupClick::Right {
                slot: Some(dest_slot as u16),
            });
            let mut missing = target - half;
            while missing > 0 {
                window.click(PickupClick::Right {
                    slot: Some(dest_slot as u16),
                });
                missing -= 1;
            }
            count = 0;
        } else {
            // one by one (right click)
            window.click(if dest_slot == -999 {
                PickupClick::RightOutside
            } else {
                PickupClick::Right {
                    slot: Some(dest_slot as u16),
                }
            });
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azalea::inventory::{
        components::Damage,
        operations::{CloneClick, PickupAllClick, QuickCraftClick},
    };

    #[test]
    fn player_inventory_slot_to_window_mapping() {
        use crate::windows::player_inventory_slot_to_window as map;
        // hotbar 0..=8 → window 36..=44
        for vanilla in 0..=8u32 {
            assert_eq!(map(vanilla), vanilla + 36);
        }
        // main inventory 9..=35 unchanged
        for vanilla in 9..=35u32 {
            assert_eq!(map(vanilla), vanilla);
        }
        // armor 36..=39 (feet→head) → window 8..=5 (feet→head)
        assert_eq!(map(36), 8);
        assert_eq!(map(37), 7);
        assert_eq!(map(38), 6);
        assert_eq!(map(39), 5);
        // offhand 40 → 45
        assert_eq!(map(40), 45);
    }

    #[test]
    fn click_operation_mode_button_mapping() {
        use ClickOperation::*;
        // mode 0: pickup
        assert!(matches!(
            click_operation(10, 0, 0).unwrap(),
            Pickup(PickupClick::Left { slot: Some(10) })
        ));
        assert!(matches!(
            click_operation(10, 1, 0).unwrap(),
            Pickup(PickupClick::Right { slot: Some(10) })
        ));
        assert!(click_operation(10, 2, 0).is_err());
        // mode 1: shift-click quick-move
        assert!(matches!(
            click_operation(10, 0, 1).unwrap(),
            QuickMove(QuickMoveClick::Left { slot: 10 })
        ));
        assert!(matches!(
            click_operation(10, 1, 1).unwrap(),
            QuickMove(QuickMoveClick::Right { slot: 10 })
        ));
        assert!(click_operation(10, 2, 1).is_err());
        // mode 2: hotbar swap — button is the hotbar index
        assert!(matches!(
            click_operation(10, 4, 2).unwrap(),
            Swap(SwapClick {
                source_slot: 10,
                target_slot: 4
            })
        ));
        // mode 3: middle/clone — upstream's modeMap maps button 2 →
        // drop-all for parity with what clickWindow callers send
        assert!(matches!(
            click_operation(10, 0, 3).unwrap(),
            Throw(ThrowClick::All { slot: 10 })
        ));
        // mode 4: drop — button 0 single, 1 all
        assert!(matches!(
            click_operation(10, 0, 4).unwrap(),
            Throw(ThrowClick::Single { slot: 10 })
        ));
        assert!(matches!(
            click_operation(10, 1, 4).unwrap(),
            Throw(ThrowClick::All { slot: 10 })
        ));
        assert!(click_operation(10, 2, 4).is_err());
        // modes 5/6 are multi-packet gestures nothing emits; 7+ invalid
        assert!(click_operation(10, 0, 5).is_err());
        assert!(click_operation(10, 0, 6).is_err());
        assert!(click_operation(10, 0, 7).is_err());
        // keep Clone/QuickCraft/PickupAll genuinely unreachable from the
        // public mapping (documents the coverage boundary)
        use azalea::inventory::operations::{QuickCraftKind, QuickCraftStatus};
        let _ = CloneClick { slot: 0 };
        let _ = QuickCraftClick {
            kind: QuickCraftKind::Left,
            status: QuickCraftStatus::Start,
        };
        let _ = PickupAllClick {
            slot: 0,
            reversed: false,
        };
    }

    #[test]
    fn equip_destination_mapping() {
        assert_eq!(dest_fixed_slot(EquipDest::Head), Some(5));
        assert_eq!(dest_fixed_slot(EquipDest::Torso), Some(6));
        assert_eq!(dest_fixed_slot(EquipDest::Legs), Some(7));
        assert_eq!(dest_fixed_slot(EquipDest::Feet), Some(8));
        assert_eq!(dest_fixed_slot(EquipDest::OffHand), Some(45));
        // Hand resolves through the live quickbar selection instead
        assert_eq!(dest_fixed_slot(EquipDest::Hand), None);
    }

    #[test]
    fn transfer_range_defaults() {
        // [start, end) with end defaulting to start + 1 mirrors upstream
        let opts = TransferOpts {
            window: None,
            kind: ItemKind::Stone,
            metadata: None,
            nbt: None,
            count: None,
            source_start: 9,
            source_end: None,
            dest_start: 27,
            dest_end: None,
        };
        assert_eq!(opts.source_end.unwrap_or(opts.source_start + 1), 10);
        assert_eq!(opts.dest_end.unwrap_or(opts.dest_start + 1), 28);
        assert!(opts.window.is_none());
    }

    fn stack(kind: ItemKind, count: i32) -> azalea::inventory::ItemStack {
        azalea::inventory::ItemStack::new(kind, count)
    }

    #[test]
    fn item_matching_and_finders() {
        use crate::windows::{find_item_in, first_empty_in};
        let slots = vec![
            stack(ItemKind::Stone, 32),
            azalea::inventory::ItemStack::Empty,
            stack(ItemKind::Stone, 64), // full
            stack(ItemKind::Dirt, 10),
            stack(ItemKind::IronSword, 1).with_component(Damage { amount: 15 }),
        ];

        // first matching kind in range
        let found = find_item_in(&slots, 0..5, ItemKind::Dirt, None, false, None).unwrap();
        assert_eq!(found.slot, 3);
        assert_eq!(found.count(), 10);

        // range is [start, end), exclusive
        assert!(find_item_in(&slots, 0..3, ItemKind::Dirt, None, false, None).is_none());
        assert!(find_item_in(&slots, 3..4, ItemKind::Dirt, None, false, None).is_some());

        // not_full skips the 64-stack but finds the 32-stack first anyway
        let merge = find_item_in(&slots, 0..5, ItemKind::Stone, None, true, None).unwrap();
        assert_eq!(merge.slot, 0);
        let merge = find_item_in(&slots, 2..3, ItemKind::Stone, None, true, None);
        assert!(merge.is_none());

        // metadata (damage) matching
        let damaged =
            find_item_in(&slots, 0..5, ItemKind::IronSword, Some(15), false, None).unwrap();
        assert_eq!(damaged.slot, 4);
        assert!(find_item_in(&slots, 0..5, ItemKind::IronSword, Some(14), false, None).is_none());
        assert_eq!(damaged.metadata(), Some(15));
        assert_eq!(damaged.durability_used(), Some(15));

        // first_empty_in
        assert_eq!(first_empty_in(&slots, 0..5), Some(1));
        assert_eq!(first_empty_in(&slots, 2..3), None);

        // Item::of empty → None; cursor slot -1
        let cursor = Item::of(stack(ItemKind::Apple, 2), -1).unwrap();
        assert_eq!(cursor.slot, -1);
        assert_eq!(cursor.kind(), ItemKind::Apple);
        assert_eq!(cursor.name(), "apple");
        assert!(Item::of(azalea::inventory::ItemStack::Empty, 0).is_none());

        // same_item ignores count but not kind or components
        let a = Item::of(stack(ItemKind::Stone, 1), 0).unwrap();
        let b = Item::of(stack(ItemKind::Stone, 64), 1).unwrap();
        assert!(a.same_item(&b));
        let c = Item::of(stack(ItemKind::Dirt, 1), 2).unwrap();
        assert!(!a.same_item(&c));
    }
}
