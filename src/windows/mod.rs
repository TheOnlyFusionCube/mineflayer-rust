//! Windows, items, and the shared window-state tracker — the
//! `prismarine-windows` / `prismarine-item` analog.
//!
//! # Slot conventions (typed windows and crafting depend on these)
//!
//! Slot indices are the protocol's absolute window indices, identical to
//! mineflayer's `window.slots` indexing and to the engine's [`Menu`] layout:
//!
//! - **Player inventory window** (always id 0): `0` craft result, `1..=4`
//!   2x2 craft grid, `5..=8` armor head/torso/legs/feet, `9..=35` main
//!   inventory, `36..=44` hotbar ([`crate::inventory::QUICK_BAR_START`]),
//!   `45` off-hand.
//! - **Container windows** (id != 0): `0..n` container contents, then the
//!   36-slot player region `n..n+36` (last 9 = hotbar).
//!   [`Window::player_range`] / [`Window::container_range`] compute these
//!   from the live menu (upstream's `inventoryStart`/`inventoryEnd`).
//! - `-999` is the "click outside" slot (drop). `-1` marks the cursor item
//!   ([`Item::slot`] of [`Window::selected_item`]).
//!
//! # Ownership model
//!
//! mineflayer's windows are shared objects closed by an explicit
//! `window.close()`; the engine's `ContainerHandle` instead closes the
//! container on `Drop`. [`Window`] ports the upstream model: it is a cheap
//! cloneable `Arc` over a [`ContainerHandleRef`] (the non-closing handle)
//! with an explicit, idempotent [`Window::close`]. Dropping a `Window`
//! does **not** close the container — `bot.current_window()` hands out
//! clones of the one open window and a dropped clone must not close it.
//!
//! # Events
//!
//! The bridge calls `observe_event` for every engine event. It decodes
//! the container packets the engine doesn't lift into `Event`s (open/close/
//! set-content/set-slot/held-slot/cooldown) into
//! [`BotEvent::WindowOpen`], [`BotEvent::WindowClose`],
//! [`BotEvent::WindowUpdateSlot`], [`BotEvent::SetWindowItems`] and
//! [`BotEvent::HeldItemChanged`]. The engine applies every inventory
//! packet and keeps the window lifecycle (early content, the last closed
//! container); `WindowPlugin` reads what each packet replaces before the
//! engine applies it, so the port keeps no copy of either. Upstream's
//! `setSlot:{id}` is `WindowUpdateSlot`'s `window_id` field;
//! `confirmTransaction{id}` has no wire on mc26 — the transaction packet
//! was removed in 1.17 and click rejects arrive
//! as corrective `set_slot`/`set_content` resyncs instead.

use std::{
    ops::Range,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;

use mineflayer_engine::{
    FormattedText,
    container::ContainerHandleRef,
    entity::inventory::Inventory,
    inventory::{DataComponentPatch, ItemStack, Menu},
    registry::builtin::{ItemKind, MenuKind},
};

use crate::{BotEvent, Error};

mod item;
mod tracker;

pub use item::Item;
pub(crate) use tracker::{WindowTracker, held_item, observe_event};

/// Records, before the engine applies each window packet, what the packet
/// replaces (see `tracker::WindowPre`).
pub(crate) struct WindowPlugin;

impl mineflayer_engine::app::Plugin for WindowPlugin {
    fn build(&self, app: &mut mineflayer_engine::app::App) {
        app.init_resource::<mineflayer_engine::connection::PacketPreApply>()
            .world_mut()
            .resource_mut::<mineflayer_engine::connection::PacketPreApply>()
            .0
            .push(tracker::record_window_pre);
    }
}

/// `bot.openBlock`/`openEntity`/`openContainer`/`inventory` produce this —
/// the upstream `Window` (and its `Chest`/`Dispenser` subclasses, which
/// add nothing over `deposit`/`withdraw`/`close` and are aliases here).
///
/// Cloning is cheap; clones share one underlying container. Closing is
/// explicit via [`Window::close`] — see the module docs for why drop does
/// not close.
#[derive(Clone)]
pub struct Window {
    pub(crate) shared: Arc<WindowShared>,
}

/// Backing state for [`Window`]. One exists per open container (and one
/// per [`Bot::inventory`](crate::Bot::inventory) call for window id 0).
pub(crate) struct WindowShared {
    /// Protocol window/container id; 0 is always the player inventory.
    pub id: i32,
    /// The menu kind from `open_screen`; `None` for the player inventory
    /// (no `MenuKind::Player` exists — the server never opens it).
    pub menu_kind: Option<MenuKind>,
    /// The window title from `open_screen`.
    pub title: Option<FormattedText>,
    /// Absolute slot index where the player-inventory region starts
    /// (upstream `window.inventoryStart`); 9 for the player window.
    pub player_start: usize,
    /// Non-closing container handle — sends clicks and the close packet.
    pub handle: ContainerHandleRef,
    /// Reads the window's slots from the engine `Inventory`.
    pub client: mineflayer_engine::Client,
    /// Set once the window is closed — by [`Window::close`] or by the
    /// server (`container_close`) — or displaced by a new `open_screen`.
    pub closed: AtomicBool,
    /// Back-reference to the bot's tracker: `close` clears
    /// `current_window` and emits `WindowClose`.
    /// Weak — the tracker holds a strong ref via `current`.
    pub tracker: Weak<Mutex<WindowTracker>>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("id", &self.shared.id)
            .field("menu_kind", &self.shared.menu_kind)
            .field("closed", &self.shared.closed.load(Ordering::Relaxed))
            .finish()
    }
}

impl Window {
    /// Build the shared handle for a server-opened window.
    pub(crate) fn new(
        id: i32,
        menu_kind: MenuKind,
        title: FormattedText,
        client: mineflayer_engine::Client,
        tracker: &Arc<Mutex<WindowTracker>>,
    ) -> Window {
        let menu = Menu::from_kind(menu_kind);
        let player_start = *menu.player_slots_range().start();
        Window {
            shared: Arc::new(WindowShared {
                id,
                menu_kind: Some(menu_kind),
                title: Some(title),
                player_start,
                handle: ContainerHandleRef::new(id, client.clone()),
                client,
                closed: AtomicBool::new(false),
                tracker: Arc::downgrade(tracker),
            }),
        }
    }

    /// The player inventory window (id 0). Created fresh per call — it is
    /// stateless over the ECS `Inventory` component, so identity doesn't
    /// matter; upstream's `bot.inventory` is the same object forever.
    pub(crate) fn player_inventory(
        client: mineflayer_engine::Client,
        tracker: &Arc<Mutex<WindowTracker>>,
    ) -> Window {
        Window {
            shared: Arc::new(WindowShared {
                id: 0,
                menu_kind: None,
                title: None,
                player_start: *mineflayer_engine::inventory::Player::INVENTORY_SLOTS.start(),
                handle: ContainerHandleRef::new(0, client.clone()),
                client,
                closed: AtomicBool::new(false),
                tracker: Arc::downgrade(tracker),
            }),
        }
    }

    /// `window.id` — 0 for the player inventory.
    pub fn id(&self) -> i32 {
        self.shared.id
    }

    /// The menu kind the server opened, e.g. [`MenuKind::Generic9x3`].
    /// `None` for the player inventory window.
    pub fn menu_kind(&self) -> Option<MenuKind> {
        self.shared.menu_kind
    }

    /// `window.title` — `None` for the player inventory (the server
    /// never sends it a title).
    pub fn title(&self) -> Option<FormattedText> {
        self.shared.title.clone()
    }

    /// Whether this window is still open (a fresh [`crate::Bot::inventory`]
    /// window is always open — the player inventory can't be closed
    /// server-side).
    pub fn is_open(&self) -> bool {
        !self.shared.closed.load(Ordering::Relaxed)
    }

    /// The live [`Menu`], or `None` if the window is closed (the menu is
    /// a clone of ECS state; writes to it do nothing — use clicks).
    /// Always resolves for id 0 — see `menu_for_id`.
    pub fn menu(&self) -> Option<Menu> {
        let inv = self.shared.client.get_component::<Inventory>()?;
        menu_for_id(&inv, self.shared.id).cloned()
    }

    /// `window.slots` — every slot in the window (container region then
    /// player region), `None` if closed. [`Window::slot`] is cheaper for
    /// single reads.
    pub fn slots(&self) -> Option<Vec<ItemStack>> {
        Some(self.menu()?.slots())
    }

    /// The [`Item`] at absolute slot `i`, `None` when empty/out of
    /// range/closed.
    pub fn slot(&self, i: i32) -> Option<Item> {
        let index = usize::try_from(i).ok()?;
        let stack = {
            let inv = self.shared.client.get_component::<Inventory>()?;
            menu_for_id(&inv, self.shared.id)?.slot(index)?.clone()
        };
        Item::of(stack, i as i16)
    }

    /// `window.selectedItem` — the stack held by the cursor, `None` when
    /// the cursor is empty. Its [`Item::slot`] is -1.
    pub fn selected_item(&self) -> Option<Item> {
        let carried = self
            .shared
            .client
            .get_component::<Inventory>()
            .map(|inv| inv.carried.clone())?;
        Item::of(carried, -1)
    }

    /// `window.inventoryStart`/`inventoryEnd` — the player-inventory
    /// region (main + hotbar) as an exclusive-end range. For the player
    /// window this is `9..45`; for a 9x3 chest `27..63`.
    pub fn player_range(&self) -> Range<usize> {
        let start = self.shared.player_start;
        start..start + 36
    }

    /// The container region — slots `0..player_range().start`. For the
    /// player window this covers crafting + armor (`0..9`).
    pub fn container_range(&self) -> Range<usize> {
        0..self.shared.player_start
    }

    /// `window.items()` — the non-empty stacks in the whole window.
    pub fn items(&self) -> Vec<Item> {
        self.items_in(0..usize::MAX)
    }

    /// `window.containerItems()` — non-empty stacks in the container
    /// region only (upstream `window.containerItems()`).
    pub fn container_items(&self) -> Vec<Item> {
        self.items_in(self.container_range())
    }

    /// `window.inventoryItems()` — non-empty stacks in the player
    /// region.
    pub fn inventory_items(&self) -> Vec<Item> {
        self.items_in(self.player_range())
    }

    fn items_in(&self, range: Range<usize>) -> Vec<Item> {
        let Some(slots) = self.slots() else {
            return Vec::new();
        };
        let end = range.end.min(slots.len());
        slots[..end]
            .iter()
            .enumerate()
            .skip(range.start)
            .filter_map(|(i, s)| Item::of(s.clone(), i as i16))
            .collect()
    }

    /// `window.findItemRange(start, end, type, metadata, notFull, nbt)` —
    /// the first matching [`Item`] in `[start, end)`.
    pub fn find_item_range(
        &self,
        range: Range<usize>,
        kind: ItemKind,
        metadata: Option<i32>,
        not_full: bool,
        nbt: Option<&DataComponentPatch>,
    ) -> Option<Item> {
        let slots = self.slots()?;
        find_item_in(&slots, range, kind, metadata, not_full, nbt)
    }

    /// `window.firstEmptySlotRange(start, end)` — first empty slot index
    /// in `[start, end)`, `None` if the range is full.
    pub fn first_empty_slot_range(&self, range: Range<usize>) -> Option<usize> {
        let slots = self.slots()?;
        first_empty_in(&slots, range)
    }

    /// `window.emptySlotCount()` — empty slots in the player region
    /// (upstream counts the whole inventory window region for the
    /// withdraw guard).
    pub fn empty_slot_count(&self) -> usize {
        let Some(slots) = self.slots() else {
            return 0;
        };
        let range = self.player_range();
        let end = range.end.min(slots.len());
        slots[range.start.min(end)..end]
            .iter()
            .filter(|s| s.is_empty())
            .count()
    }

    /// `window.deposit(itemType, metadata, count, nbt)` — move matching
    /// items from the player region into the container region. Uses
    /// [`Bot::transfer`](crate::Bot::transfer) click semantics; `None`
    /// count moves 1 item (upstream `count ?? 1`).
    pub async fn deposit(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
        nbt: Option<DataComponentPatch>,
    ) -> crate::Result<()> {
        crate::inventory::transfer(crate::inventory::TransferOpts {
            window: Some(self.clone()),
            kind,
            metadata,
            nbt,
            count,
            source_start: self.shared.player_start as i32,
            source_end: Some((self.shared.player_start + 36) as i32),
            dest_start: 0,
            dest_end: Some(self.shared.player_start as i32),
        })
        .await
    }

    /// `window.withdraw(itemType, metadata, count, nbt)` — move matching
    /// items from the container region into the player region. Errors
    /// like upstream when the inventory is already full; `None` count
    /// moves 1 item (upstream `count ?? 1`).
    pub async fn withdraw(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        count: Option<i32>,
        nbt: Option<DataComponentPatch>,
    ) -> crate::Result<()> {
        if self.empty_slot_count() == 0 {
            return Err(Error::InvalidState(
                "Unable to withdraw, Bot inventory is full.".into(),
            ));
        }
        crate::inventory::transfer(crate::inventory::TransferOpts {
            window: Some(self.clone()),
            kind,
            metadata,
            nbt,
            count,
            source_start: 0,
            source_end: Some(self.shared.player_start as i32),
            dest_start: self.shared.player_start as i32,
            dest_end: Some((self.shared.player_start + 36) as i32),
        })
        .await
    }

    /// `window.close()` — send `container_close`, clear
    /// [`crate::Bot::current_window`] if it's this window, and emit
    /// [`BotEvent::WindowClose`] exactly once. Idempotent. The engine's
    /// close handler copies the window's player region back into the
    /// player inventory (upstream `copyInventory`) and records the window
    /// for trailing syncs (upstream `lastClosedWindow`).
    ///
    /// For the player inventory (id 0) this is a client-side no-op —
    /// The engine only sends `container_close` for the open container id.
    pub fn close(&self) {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.handle.close();
        if let Some(tracker) = self.shared.tracker.upgrade() {
            let mut t = tracker.lock();
            if let Some(current) = &t.current
                && Arc::ptr_eq(current, &self.shared)
            {
                t.current = None;
            }
            let _ = t.events.send(BotEvent::WindowClose(self.clone()));
        }
    }

    /// Send a click operation on this window (`ContainerClickEvent` →
    /// local `simulate_click` + `s_container_click`). No-ops server-side
    /// if the window id isn't the open one (the engine drops it).
    pub(crate) fn click(
        &self,
        operation: impl Into<mineflayer_engine::inventory::operations::ClickOperation>,
    ) {
        self.shared.handle.click(operation);
    }
}

/// Upstream's `Chest`/`Dispenser` classes are `Window` with no extra
/// surface — aliases keep the API names without a wrapper type.
pub type Chest = Window;
/// `bot.openDispenser` returns the same generic container.
pub type Dispenser = Window;

/// Resolve the live [`Menu`] for a window id. Id 0 always reads
/// `inventory_menu` — the engine's `ContainerHandleRef` accessors gate on
/// `inv.id == self.id`, which blinds the player window while a container
/// is open (upstream keeps `bot.inventory` live during open windows; the
/// engine applies the id-0 packets to `inventory_menu`, so it stays live
/// here too).
pub(crate) fn menu_for_id(inv: &Inventory, id: i32) -> Option<&Menu> {
    if id == 0 {
        return Some(&inv.inventory_menu);
    }
    if inv.id == id {
        inv.container_menu.as_ref()
    } else {
        None
    }
}

/// First item in `slots[range]` matching the transfer predicate.
pub(crate) fn find_item_in(
    slots: &[ItemStack],
    range: Range<usize>,
    kind: ItemKind,
    metadata: Option<i32>,
    not_full: bool,
    nbt: Option<&DataComponentPatch>,
) -> Option<Item> {
    let end = range.end.min(slots.len());
    for (i, stack) in slots[range.start.min(end)..end].iter().enumerate() {
        let i = range.start + i;
        if stack.is_empty() {
            continue;
        }
        let item = Item {
            stack: stack.clone(),
            slot: i as i16,
        };
        if item.matches(kind, metadata, not_full, nbt) {
            return Some(item);
        }
    }
    None
}

/// First empty index in `slots[range]`, `None` when the range is full.
pub(crate) fn first_empty_in(slots: &[ItemStack], range: Range<usize>) -> Option<usize> {
    let end = range.end.min(slots.len());
    slots[range.start.min(end)..end]
        .iter()
        .position(|s| s.is_empty())
        .map(|p| range.start + p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `bot.inventory()` (id 0) must resolve the
    /// player menu even while a container is open — the engine's
    /// `ContainerHandleRef` gates on `inv.id == self.id`, which would
    /// return `None` for every id-0 read during an open window.
    #[test]
    fn menu_for_id_resolves_player_menu_while_container_open() {
        let inv = Inventory {
            // a container is open under window id 5
            id: 5,
            container_menu: Some(Menu::from_kind(MenuKind::Generic9x3)),
            ..Inventory::default()
        };
        // id 0 still resolves to the 46-slot player menu
        let menu = menu_for_id(&inv, 0).unwrap();
        assert_eq!(menu.len(), Menu::Player(Default::default()).len());
        assert_eq!(menu.len(), 46);
        // the open container resolves by its id
        assert_eq!(menu_for_id(&inv, 5).unwrap().len(), 27 + 36);
        // an unrelated id doesn't
        assert!(menu_for_id(&inv, 6).is_none());
        // and once the container is closed, its id goes blind again
        let closed = Inventory {
            id: 0,
            container_menu: None,
            ..Inventory::default()
        };
        assert!(menu_for_id(&closed, 0).is_some());
        assert!(menu_for_id(&closed, 5).is_none());
    }
}
