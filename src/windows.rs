//! Windows, items, and the shared window-state tracker — the
//! `prismarine-windows` / `prismarine-item` analog.
//!
//! # Slot conventions (load-bearing for U9 typed windows and U10 crafting)
//!
//! Slot indices are the protocol's absolute window indices, identical to
//! mineflayer's `window.slots` indexing and to azalea's [`Menu`] layout:
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
//! `window.close()`; azalea's `ContainerHandle` instead closes the
//! container on `Drop`. [`Window`] ports the upstream model: it is a cheap
//! cloneable `Arc` over a [`ContainerHandleRef`] (the non-closing handle)
//! with an explicit, idempotent [`Window::close`]. Dropping a `Window`
//! does **not** close the container — `bot.current_window()` hands out
//! clones of the one open window and a dropped clone must not close it.
//!
//! # Events
//!
//! The bridge calls `observe_event` for every azalea event. It decodes
//! the container packets azalea doesn't lift into `Event`s (open/close/
//! set-content/set-slot/held-slot/cooldown) into
//! [`BotEvent::WindowOpen`], [`BotEvent::WindowClose`],
//! [`BotEvent::WindowUpdateSlot`], [`BotEvent::SetWindowItems`] and
//! [`BotEvent::HeldItemChanged`], keeps `WindowTracker` current, and
//! patches the two azalea gaps that matter for parity:
//! `container_set_content`'s `carried_item` (azalea ignores it, so the
//! cursor would desync) and `c_set_player_inventory` (azalea drops the
//! packet entirely). Upstream's `setSlot:{id}` is `WindowUpdateSlot`'s
//! `window_id` field; `confirmTransaction{id}` has no wire on mc26 —
//! the transaction packet was removed in 1.17 and click rejects arrive
//! as corrective `set_slot`/`set_content` resyncs instead.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use azalea::{
    Event, FormattedText,
    container::ContainerHandleRef,
    core::entity_id::MinecraftEntityId,
    entity::inventory::Inventory,
    inventory::{DataComponentPatch, ItemStack, Menu, item::MaxStackSizeExt},
    protocol::packets::game::ClientboundGamePacket,
    registry::builtin::{ItemKind, MenuKind},
};
use tokio::sync::{Notify, broadcast};

use crate::{BotEvent, Error};

/// A stack of items in a window slot — the `prismarine-item` analog.
///
/// Wraps [`ItemStack`] with the absolute window slot index it occupies.
/// `slot == -1` marks the cursor (carried) stack, which lives in no slot.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// The protocol item stack (kind, count, component patch).
    pub stack: ItemStack,
    /// Absolute window slot index, or -1 for the carried/cursor stack.
    pub slot: i16,
}

impl Item {
    /// Wrap a stack; `None` when the stack is empty (upstream `null`).
    pub fn of(stack: ItemStack, slot: i16) -> Option<Item> {
        if stack.is_empty() {
            None
        } else {
            Some(Item { stack, slot })
        }
    }

    /// `item.type` analog — the item kind, e.g. [`ItemKind::Stone`]. The
    /// numeric mcdata id has no azalea equivalent; the enum is richer.
    pub fn kind(&self) -> ItemKind {
        self.stack.kind()
    }

    /// `item.name` — e.g. `"stone"` (the identifier path, no namespace).
    pub fn name(&self) -> &'static str {
        // to_str() is "minecraft:<name>"; item kinds are all minecraft:
        &self.kind().to_str()["minecraft:".len()..]
    }

    /// `item.displayName` — the `custom_name` component when present,
    /// else the item name.
    pub fn display_name(&self) -> String {
        self.stack
            .get_component::<azalea::inventory::components::CustomName>()
            .map(|c| c.name.to_string())
            .unwrap_or_else(|| self.name().to_string())
    }

    /// `item.count` — stack count (0 when absent).
    pub fn count(&self) -> i32 {
        self.stack.count()
    }

    /// `item.stackSize` — the max stack size for this kind.
    pub fn stack_size(&self) -> i32 {
        self.kind().max_stack_size()
    }

    /// `item.metadata` / `item.durabilityUsed` — the `damage` data
    /// component (`0` for undamaged tools, `None` for undamageable items).
    pub fn metadata(&self) -> Option<i32> {
        self.durability_used()
    }

    /// `item.durabilityUsed` — damage already taken.
    pub fn durability_used(&self) -> Option<i32> {
        self.stack
            .get_component::<azalea::inventory::components::Damage>()
            .map(|d| d.amount)
    }

    /// `item.maxDurability` — the `max_damage` component, `None` for
    /// items without durability.
    pub fn max_durability(&self) -> Option<i32> {
        self.stack
            .get_component::<azalea::inventory::components::MaxDamage>()
            .map(|d| d.amount)
    }

    /// `item.nbt` analog — the `custom_data` component. Modern items have
    /// component patches rather than free-form NBT; use
    /// [`Item::component_patch`] for the full set.
    pub fn custom_data(&self) -> Option<azalea::inventory::components::CustomData> {
        self.stack
            .get_component::<azalea::inventory::components::CustomData>()
            .map(|c| c.into_owned())
    }

    /// The raw component patch — upstream's `nbt` in component form.
    pub fn component_patch(&self) -> &DataComponentPatch {
        self.stack.component_patch()
    }

    /// Whether this and `other` are the same kind with the same
    /// components, ignoring count (upstream `Item.equal(x, false)`).
    pub fn same_item(&self, other: &Item) -> bool {
        match (self.stack.as_present(), other.stack.as_present()) {
            (Some(a), Some(b)) => a.is_same_item_and_components(b),
            _ => false,
        }
    }

    /// Whether this stack satisfies a transfer-style matcher: same kind,
    /// optional `metadata` (damage) equality, optional `nbt` (component
    /// patch) equality, optional `not_full` (room left on the stack).
    pub(crate) fn matches(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        not_full: bool,
        nbt: Option<&DataComponentPatch>,
    ) -> bool {
        if self.kind() != kind {
            return false;
        }
        if let Some(meta) = metadata
            && self.durability_used() != Some(meta)
        {
            return false;
        }
        if let Some(patch) = nbt
            && self.component_patch() != patch
        {
            return false;
        }
        !(not_full && self.count() >= self.stack_size())
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
    /// Total menu slot count (the `window_items` shape check upstream
    /// uses for `lastClosedWindow` trailing-sync detection).
    pub len: usize,
    /// Non-closing container handle — sends clicks and the close packet.
    pub handle: ContainerHandleRef,
    /// Kept for ECS access (`Window::close`'s `copyInventory`).
    pub client: azalea::Client,
    /// Set once the window is closed — by [`Window::close`] or by the
    /// server (`container_close`) — or displaced by a new `open_screen`.
    pub closed: AtomicBool,
    /// Back-reference to the bot's tracker: `close` clears
    /// `current_window`, seeds `last_closed`, and emits `WindowClose`.
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
        client: azalea::Client,
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
                len: menu.len(),
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
        client: azalea::Client,
        tracker: &Arc<Mutex<WindowTracker>>,
    ) -> Window {
        Window {
            shared: Arc::new(WindowShared {
                id: 0,
                menu_kind: None,
                title: None,
                player_start: *azalea::inventory::Player::INVENTORY_SLOTS.start(),
                len: Menu::Player(Default::default()).len(),
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
        menu_for_id(&inv, self.shared.id)
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
        let menu = self.menu()?;
        if i < 0 || i as usize >= menu.len() {
            return None;
        }
        Item::of(menu.slot(i as usize).cloned()?, i as i16)
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
            return Err(Error::Operation(
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

    /// `window.close()` — copy the window's player region back into the
    /// inventory model (upstream `copyInventory`), send `container_close`,
    /// clear [`crate::Bot::current_window`] if it's this window, and emit
    /// [`BotEvent::WindowClose`] exactly once. Idempotent.
    ///
    /// For the player inventory (id 0) this is a client-side no-op —
    /// azalea only sends `container_close` for the open container id.
    pub fn close(&self) {
        if self.shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.shared.id != 0 {
            // copyInventory: while the window was open the server's
            // player-inventory updates landed on the container menu, so
            // the window's player region is fresher than inventory_menu.
            // Copy it before azalea's close handler drops the menu.
            let mut ecs = self.shared.client.ecs.write();
            if let Some(mut inv) = ecs.get_mut::<Inventory>(self.shared.client.entity) {
                let inv = &mut *inv;
                if let Some(menu) = inv.container_menu.as_ref() {
                    let range = menu.player_slots_range();
                    let stacks: Vec<ItemStack> = range
                        .clone()
                        .filter_map(|i| menu.slot(i).cloned())
                        .collect();
                    for (off, stack) in stacks.into_iter().enumerate() {
                        let inv_slot = off + 9;
                        if let Some(dst) = inv.inventory_menu.slot_mut(inv_slot) {
                            *dst = stack;
                        }
                    }
                }
            }
            drop(ecs);
        }
        self.shared.handle.close();
        if let Some(tracker) = self.shared.tracker.upgrade() {
            let mut t = tracker.lock().unwrap();
            // drop the slot snapshot — a reused window id must not see
            // this window's contents as `old` in WindowUpdateSlot
            t.slots.remove(&self.shared.id);
            if let Some(current) = &t.current
                && Arc::ptr_eq(current, &self.shared)
            {
                t.last_closed = Some((self.shared.id, self.shared.player_start, self.shared.len));
                t.current = None;
            }
            let _ = t.events.send(BotEvent::WindowClose(self.clone()));
        }
    }

    /// Send a click operation on this window (`ContainerClickEvent` →
    /// local `simulate_click` + `s_container_click`). No-ops server-side
    /// if the window id isn't the open one (azalea drops it).
    pub(crate) fn click(
        &self,
        operation: impl Into<azalea::inventory::operations::ClickOperation>,
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
/// `inventory_menu` — azalea's `ContainerHandleRef` accessors gate on
/// `inv.id == self.id`, which blinds the player window while a container
/// is open (upstream keeps `bot.inventory` live during open windows via
/// the id-0 packet writes our observer patches into `inventory_menu`).
pub(crate) fn menu_for_id(inv: &Inventory, id: i32) -> Option<Menu> {
    if id == 0 {
        return Some(inv.inventory_menu.clone());
    }
    if inv.id == id {
        inv.container_menu.clone()
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

// ---------------------------------------------------------------------------
// Shared tracker — lives in `Bot`, written by the bridge's `observe_event`.
// ---------------------------------------------------------------------------

/// A window announced by `open_screen` that is still waiting for its
/// first `container_set_content` — upstream only emits `windowOpen` once
/// the slot data has arrived.
pub(crate) struct PendingWindow {
    pub id: i32,
    pub menu_kind: MenuKind,
    pub title: FormattedText,
}

/// Inventory/window state shared between `Bot` methods and the bridge —
/// the analog of upstream's `bot.currentWindow` plus the click/eat
/// bookkeeping in inventory.js.
pub(crate) struct WindowTracker {
    /// The currently open non-inventory window (id != 0).
    pub current: Option<Arc<WindowShared>>,
    /// `open_screen` seen, waiting for first `container_set_content`.
    pub pending: Option<PendingWindow>,
    /// Slot data that arrived for a not-yet-open window id (the upstream
    /// `windowItems` buffer — consumed by the matching `open_screen`).
    pub early_items: Option<(i32, Vec<ItemStack>)>,
    /// Last-seen slot contents per window id — the `old` side of
    /// `WindowUpdateSlot`. Keyed by window id; 0 = player inventory.
    pub slots: HashMap<i32, Vec<ItemStack>>,
    /// `(id, player_start, menu_len)` of the last window we closed
    /// client-side — server ticks can emit trailing syncs under the
    /// closed id; its player-region slots still describe the player
    /// inventory and are applied there (upstream `lastClosedWindow`; the
    /// shape check needs the menu's total length).
    pub last_closed: Option<(i32, usize, usize)>,
    /// Last-seen held-item signature `(kind, count)` — upstream only
    /// cancels eating on a type/count change, not on every
    /// `heldItemChanged`.
    pub last_held: Option<(ItemKind, i32)>,
    /// `bot.usingHeldItem`.
    pub using_held_item: bool,
    /// Bumped per `consume` call — a stale waiter resolves as cancelled,
    /// like upstream's `eatingTask.cancel`.
    pub eat_generation: u64,
    /// Wakes `consume` on eat-finish (entity status 9, matching cooldown,
    /// or a held-item type/count change).
    pub eat_notify: Arc<Notify>,
    /// The quickbar slot we've told the server (or it told us) —
    /// `set_selected_hotbar_slot`'s component write lands a tick late,
    /// so reads route through here (upstream `carriedIndex`).
    pub selected_hotbar: Option<u8>,
    /// LRU rotation for `equip`'s quickbar fallback slot (upstream
    /// `nextQuickBarSlot`).
    pub next_quick_bar_slot: u8,
    /// Bot event sender — `Window::close` emits `WindowClose` itself.
    pub events: broadcast::Sender<BotEvent>,
}

impl WindowTracker {
    pub(crate) fn new(events: broadcast::Sender<BotEvent>) -> Self {
        Self {
            current: None,
            pending: None,
            early_items: None,
            slots: HashMap::new(),
            last_closed: None,
            last_held: None,
            using_held_item: false,
            eat_generation: 0,
            eat_notify: Arc::new(Notify::new()),
            selected_hotbar: None,
            next_quick_bar_slot: 0,
            events,
        }
    }

    /// Held-item signature change → finish any in-flight eat and clear
    /// `using_held_item` (the upstream `heldItemChanged` listener only
    /// reacts to type/count changes).
    fn held_sig_changed(&mut self, client: &azalea::Client) {
        let sig = held_signature(client, self.selected_hotbar);
        if sig != self.last_held {
            self.last_held = sig;
            self.using_held_item = false;
            self.eat_notify.notify_waiters();
        }
    }
}

/// `(kind, count)` of the currently held stack, `None` when empty or the
/// component is missing (pre-login). `sel` overrides the ECS selection
/// while `set_selected_hotbar_slot`'s component write is still pending.
fn held_signature(client: &azalea::Client, sel: Option<u8>) -> Option<(ItemKind, i32)> {
    let inv = client.get_component::<Inventory>()?;
    let sel = sel.unwrap_or(inv.selected_hotbar_slot);
    let stack = inv
        .inventory_menu
        .slot(usize::from(sel) + crate::inventory::QUICK_BAR_START)?;
    if stack.is_empty() {
        None
    } else {
        Some((stack.kind(), stack.count()))
    }
}

/// The held stack as an [`Item`] with its absolute window slot, `None`
/// when the hand is empty or the component is gone.
pub(crate) fn held_item(client: &azalea::Client, sel: u8) -> Option<Item> {
    let inv = client.get_component::<Inventory>()?;
    let index = crate::inventory::QUICK_BAR_START + usize::from(sel);
    Item::of(inv.inventory_menu.slot(index)?.clone(), index as i16)
}

/// `c_set_player_inventory` uses vanilla `Inventory` indices, not window
/// slots (upstream `playerInventorySlotToWindow`): `0..=8` hotbar →
/// `36..=44`, `36..=39` armor feet→head → `8..=5`, `40` off-hand → `45`,
/// `9..=35` main inventory unchanged.
pub(crate) fn player_inventory_slot_to_window(slot: u32) -> u32 {
    if slot <= 8 {
        slot + 36
    } else if (36..=39).contains(&slot) {
        44 - slot
    } else if slot == 40 {
        45 // offhand
    } else {
        slot
    }
}

/// Decode one azalea [`Event`] into window-domain [`BotEvent`]s — the
/// inventory-side analog of `entity::observe_event`. Runs after azalea's
/// own packet handlers, so the ECS `Inventory` component already reflects
/// the packet; the pieces azalea drops (`carried_item` on set-content,
/// `set_player_inventory`, non-hotbar `set_slot` writes while a container
/// is open, slot data sent before `open_screen`, trailing syncs under a
/// closed id) are patched into the ECS here.
pub(crate) fn observe_event(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &azalea::Client,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    let sel = |t: &WindowTracker| {
        t.selected_hotbar.unwrap_or_else(|| {
            client
                .get_component::<Inventory>()
                .map(|i| i.selected_hotbar_slot)
                .unwrap_or(0)
        })
    };
    match &**packet {
        ClientboundGamePacket::OpenScreen(p) => {
            let mut t = tracker.lock().unwrap();
            // a new screen displaces the previous open window without a
            // close packet — mark it closed like upstream replacing
            // `currentWindow`, and clear `lastClosedWindow` (upstream's
            // open_window handler does).
            if let Some(current) = t.current.take() {
                current.closed.store(true, Ordering::SeqCst);
                t.slots.remove(&current.id);
            }
            t.last_closed = None;
            let early_items = t
                .early_items
                .take_if(|(id, _)| *id == p.container_id)
                .map(|(_, items)| items);
            if let Some(items) = early_items {
                // slot data arrived before the open packet (upstream's
                // prepareWindow sync branch). azalea dropped that early
                // content — write it into the fresh menu now.
                {
                    let mut ecs = client.ecs.write();
                    if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity)
                        && let Some(menu) = inv.container_menu.as_mut()
                    {
                        for (i, stack) in items.iter().enumerate() {
                            if let Some(slot) = menu.slot_mut(i) {
                                *slot = stack.clone();
                            }
                        }
                    }
                }
                let window = Window::new(
                    p.container_id,
                    p.menu_type,
                    p.title.clone(),
                    client.clone(),
                    tracker,
                );
                t.current = Some(window.shared.clone());
                let mut out = Vec::new();
                for (i, stack) in items.iter().enumerate() {
                    let old = snapshot_set(&mut t, p.container_id, i, stack.clone());
                    out.push(BotEvent::WindowUpdateSlot {
                        window_id: p.container_id,
                        slot: i as u16,
                        old,
                        new: Item::of(stack.clone(), i as i16),
                    });
                }
                t.held_sig_changed(client);
                out.push(BotEvent::HeldItemChanged(held_item(client, sel(&t))));
                out.push(BotEvent::WindowOpen(window));
                out
            } else {
                t.pending = Some(PendingWindow {
                    id: p.container_id,
                    menu_kind: p.menu_type,
                    title: p.title.clone(),
                });
                Vec::new()
            }
        }
        ClientboundGamePacket::ContainerSetContent(p) => set_content_events(tracker, client, p),
        ClientboundGamePacket::ContainerSetSlot(p) => set_slot_events(tracker, client, p),
        ClientboundGamePacket::ContainerClose(p) => {
            let mut t = tracker.lock().unwrap();
            // the server may close a pending window before content ever
            // arrived — drop it only if the close names the same id
            t.pending.take_if(|w| w.id == p.container_id);
            let Some(shared) = t.current.take() else {
                // server acking our own close() — already emitted
                return Vec::new();
            };
            shared.closed.store(true, Ordering::SeqCst);
            // `last_closed` is only seeded on *client* close (upstream
            // sets it in closeWindow); a server close of this id would
            // otherwise misroute early items of a same-id successor
            t.slots.remove(&shared.id);
            vec![BotEvent::WindowClose(Window {
                shared: shared.clone(),
            })]
        }
        ClientboundGamePacket::SetHeldSlot(p) => {
            if p.slot > 8 {
                return Vec::new();
            }
            // upstream's held_item_slot handler only emits when the slot
            // actually changed
            let mut t = tracker.lock().unwrap();
            let changed = u32::from(sel(&t)) != p.slot;
            t.selected_hotbar = Some(p.slot as u8);
            t.held_sig_changed(client);
            if changed {
                vec![BotEvent::HeldItemChanged(held_item(client, p.slot as u8))]
            } else {
                Vec::new()
            }
        }
        ClientboundGamePacket::SetPlayerInventory(p) => {
            let win_slot = player_inventory_slot_to_window(p.slot);
            // azalea drops this packet entirely — apply it ourselves to
            // the player menu, then report it like a `set_slot`.
            {
                let mut ecs = client.ecs.write();
                if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity)
                    && let Some(slot) = inv.inventory_menu.slot_mut(win_slot as usize)
                {
                    *slot = p.contents.clone();
                }
            }
            let mut t = tracker.lock().unwrap();
            let old = snapshot_set(&mut t, 0, win_slot as usize, p.contents.clone());
            t.held_sig_changed(client);
            vec![
                BotEvent::WindowUpdateSlot {
                    window_id: 0,
                    slot: win_slot as u16,
                    old,
                    new: Item::of(p.contents.clone(), win_slot as i16),
                },
                BotEvent::HeldItemChanged(held_item(client, sel(&t))),
            ]
        }
        ClientboundGamePacket::EntityEvent(p) => {
            let mut t = tracker.lock().unwrap();
            // upstream clears usingHeldItem on every entity_status packet
            t.using_held_item = false;
            // entity status 9 on our own entity = "finish using item"
            let is_self = client
                .get_component::<MinecraftEntityId>()
                .map(|id| *id == p.entity_id)
                .unwrap_or(false);
            if p.event_id == 9 && is_self {
                t.eat_notify.notify_waiters();
            }
            Vec::new()
        }
        ClientboundGamePacket::Cooldown(p) => {
            // upstream finishes eating when the cooldown is for the held
            // item — or whenever heldItem is null
            let mut t = tracker.lock().unwrap();
            let held = held_signature(client, t.selected_hotbar);
            if held.is_some_and(|(kind, _)| kind != p.item) {
                return Vec::new();
            }
            t.using_held_item = false;
            t.eat_notify.notify_waiters();
            Vec::new()
        }
        ClientboundGamePacket::Respawn(_) | ClientboundGamePacket::Login(_) => {
            // a new server-side player entity restarts the window id
            // counter; pending/trailing bookkeeping is stale (upstream
            // clears windowItems + lastClosedWindow on respawn/login).
            let mut t = tracker.lock().unwrap();
            t.pending = None;
            t.early_items = None;
            t.last_closed = None;
            t.slots.clear();
            // the override only bridges set_selected_hotbar_slot's
            // tick-late component write — a fresh server-side player makes
            // any stale value wrong (upstream keeps `quickBarSlot`, and the
            // ECS slot still reports it via the fallback path)
            t.selected_hotbar = None;
            let mut out = Vec::new();
            if matches!(&**packet, ClientboundGamePacket::Login(_))
                && let Some(shared) = t.current.take()
            {
                // switching subservers closes the open window (upstream
                // emits windowClose on login)
                shared.closed.store(true, Ordering::SeqCst);
                out.push(BotEvent::WindowClose(Window { shared }));
            }
            out
        }
        _ => Vec::new(),
    }
}

/// `container_set_content` — full slot refresh for one window. Completes
/// a pending window into `current` + `windowOpen` (upstream's
/// `setWindowItems` gate), then emits one `WindowUpdateSlot` per reported
/// slot, like upstream calling `updateSlot` for each. Resolved packets
/// (window 0 or the open container) end in `SetWindowItems` — upstream's
/// `setWindowItems:{id}`; buffered (`early_items`) and `last_closed`
/// remaps return before upstream's emit, so they emit nothing here.
fn set_content_events(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &azalea::Client,
    p: &azalea::protocol::packets::game::c_container_set_content::ClientboundContainerSetContent,
) -> Vec<BotEvent> {
    // azalea ignores `carried_item` on this packet — patch the cursor so
    // click packets carry the right stack.
    {
        let mut ecs = client.ecs.write();
        if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity) {
            inv.carried = p.carried_item.clone();
        }
    }

    let mut t = tracker.lock().unwrap();
    let sel = t.selected_hotbar.unwrap_or_else(|| {
        client
            .get_component::<Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    });
    let mut out = Vec::new();

    if p.container_id == 0 {
        for (i, stack) in p.items.iter().enumerate() {
            let old = snapshot_set(&mut t, 0, i, stack.clone());
            out.push(BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: i as u16,
                old,
                new: Item::of(stack.clone(), i as i16),
            });
        }
        // upstream calls updateHeldItem() after every window_items,
        // then emits `setWindowItems:{id}`
        t.held_sig_changed(client);
        out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
        out.push(BotEvent::SetWindowItems { window_id: 0 });
        return out;
    }

    if let Some(pending) = t.pending.take_if(|w| w.id == p.container_id) {
        let window = Window::new(
            pending.id,
            pending.menu_kind,
            pending.title,
            client.clone(),
            tracker,
        );
        t.current = Some(window.shared.clone());
        for (i, stack) in p.items.iter().enumerate() {
            let old = snapshot_set(&mut t, p.container_id, i, stack.clone());
            out.push(BotEvent::WindowUpdateSlot {
                window_id: p.container_id,
                slot: i as u16,
                old,
                new: Item::of(stack.clone(), i as i16),
            });
        }
        t.held_sig_changed(client);
        out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
        // upstream emits `setWindowItems:{id}` here; the once-listener
        // it registered in prepareWindow then fires `windowOpen`
        out.push(BotEvent::SetWindowItems {
            window_id: p.container_id,
        });
        out.push(BotEvent::WindowOpen(window));
        return out;
    }

    if t.current.as_ref().is_some_and(|c| c.id == p.container_id) {
        // resync of the open window — upstream's resolved-window path
        // also runs updateHeldItem() before `setWindowItems:{id}`
        for (i, stack) in p.items.iter().enumerate() {
            let old = snapshot_set(&mut t, p.container_id, i, stack.clone());
            out.push(BotEvent::WindowUpdateSlot {
                window_id: p.container_id,
                slot: i as u16,
                old,
                new: Item::of(stack.clone(), i as i16),
            });
        }
        t.held_sig_changed(client);
        out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
        out.push(BotEvent::SetWindowItems {
            window_id: p.container_id,
        });
        return out;
    }

    if let Some((closed_id, player_start, menu_len)) = t.last_closed
        && p.container_id == closed_id
        && p.items.len() == menu_len
    {
        // Trailing bulk sync under a window we closed client-side: its
        // player region still describes the player inventory — remap
        // into window 0 like upstream (the length check keeps an early
        // sync for a *different* same-id window from being misrouted).
        for (i, stack) in p.items.iter().enumerate().skip(player_start) {
            let inv_slot = i - player_start + 9;
            {
                let mut ecs = client.ecs.write();
                if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity)
                    && let Some(slot) = inv.inventory_menu.slot_mut(inv_slot)
                {
                    *slot = stack.clone();
                }
            }
            let old = snapshot_set(&mut t, 0, inv_slot, stack.clone());
            out.push(BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: inv_slot as u16,
                old,
                new: Item::of(stack.clone(), inv_slot as i16),
            });
        }
        return out;
    }

    // content for a window id we've never seen — buffer it; a matching
    // `open_screen` consumes it (upstream `windowItems`).
    t.early_items = Some((p.container_id, p.items.clone()));
    out
}

/// `container_set_slot` — single-slot write. `container_id` -1 is the
/// cursor (applied by azalea, no event upstream), -2 and 0 hit the player
/// window, anything else is the open container (or a trailing sync under
/// `last_closed`).
fn set_slot_events(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &azalea::Client,
    p: &azalea::protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot,
) -> Vec<BotEvent> {
    let mut t = tracker.lock().unwrap();
    let sel = t.selected_hotbar.unwrap_or_else(|| {
        client
            .get_component::<Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    });
    let mut out = Vec::new();

    match p.container_id {
        -1 => {
            // cursor item — azalea applies it; upstream emits nothing.
            t.held_sig_changed(client);
        }
        -2 | 0 => {
            // While a container is open azalea only applies id-0 writes
            // to hotbar slots — patch the rest into the player menu so
            // the model matches the server's write (upstream applies
            // every id-0 set_slot to bot.inventory).
            if p.container_id == 0
                && t.current.is_some()
                && !azalea::inventory::Player::is_hotbar_slot(usize::from(p.slot))
            {
                let mut ecs = client.ecs.write();
                if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity)
                    && let Some(slot) = inv.inventory_menu.slot_mut(usize::from(p.slot))
                {
                    *slot = p.item_stack.clone();
                }
            }
            let old = snapshot_set(&mut t, 0, p.slot as usize, p.item_stack.clone());
            out.push(BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: p.slot,
                old,
                new: Item::of(p.item_stack.clone(), p.slot as i16),
            });
            t.held_sig_changed(client);
            out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
        }
        id => {
            if t.current.as_ref().is_some_and(|c| c.id == id) {
                let old = snapshot_set(&mut t, id, p.slot as usize, p.item_stack.clone());
                out.push(BotEvent::WindowUpdateSlot {
                    window_id: id,
                    slot: p.slot,
                    old,
                    new: Item::of(p.item_stack.clone(), p.slot as i16),
                });
            } else if let Some((closed_id, player_start, _)) = t.last_closed
                && id == closed_id
                && (p.slot as usize) >= player_start
                && (p.slot as usize) < player_start + 36
            {
                // Trailing sync under a window we closed client-side:
                // its player region still describes the player
                // inventory — remap into window 0 like upstream.
                let inv_slot = p.slot as usize - player_start + 9;
                {
                    let mut ecs = client.ecs.write();
                    if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity)
                        && let Some(slot) = inv.inventory_menu.slot_mut(inv_slot)
                    {
                        *slot = p.item_stack.clone();
                    }
                }
                let old = snapshot_set(&mut t, 0, inv_slot, p.item_stack.clone());
                out.push(BotEvent::WindowUpdateSlot {
                    window_id: 0,
                    slot: inv_slot as u16,
                    old,
                    new: Item::of(p.item_stack.clone(), inv_slot as i16),
                });
            }
        }
    }
    out
}

/// Record `new` into the tracked snapshot for `window_id[slot]`,
/// returning the previous value as an [`Item`] (empty → `None`).
fn snapshot_set(
    t: &mut WindowTracker,
    window_id: i32,
    slot: usize,
    new: ItemStack,
) -> Option<Item> {
    let slots = t.slots.entry(window_id).or_default();
    if slots.len() <= slot {
        slots.resize(slot + 1, ItemStack::Empty);
    }
    let old = std::mem::replace(&mut slots[slot], new);
    Item::of(old, slot as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C2 regression: `bot.inventory()` (id 0) must resolve the
    /// player menu even while a container is open — azalea's
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

    // — packet→event decode: the per-window-id events upstream's
    // inventory.js emits (`setSlot:{id}`, `setWindowItems:{id}`) —

    use azalea::protocol::packets::game::{
        c_container_set_content::ClientboundContainerSetContent,
        c_container_set_slot::ClientboundContainerSetSlot, c_open_screen::ClientboundOpenScreen,
    };

    fn packet_event(p: ClientboundGamePacket) -> Event {
        Event::Packet(Arc::new(p))
    }

    fn tracker() -> Arc<Mutex<WindowTracker>> {
        let (events, _rx) = broadcast::channel(8);
        Arc::new(Mutex::new(WindowTracker::new(events)))
    }

    fn client() -> azalea::Client {
        crate::test_util::live_entity_client(Inventory::default())
    }

    fn open_screen(id: i32) -> Event {
        packet_event(ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
            container_id: id,
            menu_type: MenuKind::Generic9x3,
            title: azalea::FormattedText::from("chest"),
        }))
    }

    fn set_content(id: i32, items: Vec<ItemStack>) -> Event {
        packet_event(ClientboundGamePacket::ContainerSetContent(
            ClientboundContainerSetContent {
                container_id: id,
                state_id: 0,
                items,
                carried_item: ItemStack::Empty,
            },
        ))
    }

    fn set_slot(id: i32, slot: u16, stack: ItemStack) -> Event {
        packet_event(ClientboundGamePacket::ContainerSetSlot(
            ClientboundContainerSetSlot {
                container_id: id,
                state_id: 0,
                slot,
                item_stack: stack,
            },
        ))
    }

    /// Upstream `setSlot:{windowId}` — every resolved `set_slot` write
    /// reports `(window_id, slot, old, new)`.
    #[test]
    fn set_slot_reports_per_window_slot_update() {
        let t = tracker();
        let client = client();
        observe_event(&t, &client, &open_screen(3));
        observe_event(&t, &client, &set_content(3, vec![ItemStack::Empty; 63]));
        let out = observe_event(
            &t,
            &client,
            &set_slot(3, 2, ItemStack::new(ItemKind::Stone, 5)),
        );
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 3,
                slot: 2,
                old: None,
                new: Some(item),
            } if item.kind() == ItemKind::Stone && item.count() == 5
        )));
        // set_player_inventory and id-0/-2 writes report under window 0
        let out = observe_event(
            &t,
            &client,
            &set_slot(0, 40, ItemStack::new(ItemKind::Dirt, 1)),
        );
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: 40,
                ..
            }
        )));
    }

    /// Upstream `setWindowItems:{windowId}` — fires once per resolved
    /// `container_set_content`, after the per-slot updates and
    /// `heldItemChanged`, before `windowOpen` on first fill.
    #[test]
    fn set_content_emits_set_window_items_with_upstream_ordering() {
        let t = tracker();
        let client = client();
        assert!(observe_event(&t, &client, &open_screen(3)).is_empty());
        let items: Vec<ItemStack> = (0..63)
            .map(|i| {
                if i == 0 {
                    ItemStack::new(ItemKind::Stone, 1)
                } else {
                    ItemStack::Empty
                }
            })
            .collect();
        let out = observe_event(&t, &client, &set_content(3, items));
        let pos =
            |pred: &dyn Fn(&BotEvent) -> bool| out.iter().position(pred).unwrap_or(usize::MAX);
        let last_slot = out
            .iter()
            .rposition(|e| matches!(e, BotEvent::WindowUpdateSlot { window_id: 3, .. }))
            .unwrap();
        let held = pos(&|e| matches!(e, BotEvent::HeldItemChanged(_)));
        let items_set = pos(&|e| matches!(e, BotEvent::SetWindowItems { window_id: 3 }));
        let opened = pos(&|e| matches!(e, BotEvent::WindowOpen(w) if w.id() == 3));
        assert!(last_slot < held && held < items_set && items_set < opened);

        // a resync of the still-open window emits it again (upstream's
        // resolved-window path runs updateHeldItem + setWindowItems too)
        let out = observe_event(&t, &client, &set_content(3, vec![ItemStack::Empty; 63]));
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::HeldItemChanged(_)))
        );
        assert!(matches!(
            out.last(),
            Some(BotEvent::SetWindowItems { window_id: 3 })
        ));

        // window 0 resolves too (upstream `bot.inventory`)
        let out = observe_event(&t, &client, &set_content(0, vec![ItemStack::Empty; 46]));
        assert!(matches!(
            out.last(),
            Some(BotEvent::SetWindowItems { window_id: 0 })
        ));
    }

    /// Slot data ahead of `open_screen` is buffered (`early_items`,
    /// upstream's `windowItems`); the consuming `open_screen` emits
    /// `windowOpen` without `setWindowItems` — upstream's prepareWindow
    /// sync branch emits only `updateSlot` per item.
    #[test]
    fn buffered_items_open_without_set_window_items() {
        let t = tracker();
        let client = client();
        assert!(observe_event(&t, &client, &set_content(7, vec![ItemStack::Empty; 63])).is_empty());
        let out = observe_event(&t, &client, &open_screen(7));
        assert!(
            !out.iter()
                .any(|e| matches!(e, BotEvent::SetWindowItems { .. }))
        );
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::WindowUpdateSlot { window_id: 7, .. }))
        );
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowOpen(w) if w.id() == 7
        )));
    }

    /// A trailing bulk sync under a client-closed id remaps its player
    /// region into window 0 (upstream `lastClosedWindow`) and never
    /// emits `setWindowItems` — upstream returns before the emit.
    #[test]
    fn trailing_content_remaps_to_inventory_without_set_window_items() {
        let t = tracker();
        let client = client();
        observe_event(&t, &client, &open_screen(9));
        let out = observe_event(&t, &client, &set_content(9, vec![ItemStack::Empty; 63]));
        let window = out
            .iter()
            .find_map(|e| match e {
                BotEvent::WindowOpen(w) => Some(w.clone()),
                _ => None,
            })
            .unwrap();
        window.close();
        assert_eq!(t.lock().unwrap().last_closed, Some((9, 27, 63)));

        let mut items = vec![ItemStack::Empty; 63];
        items[27] = ItemStack::new(ItemKind::Stone, 2);
        let out = observe_event(&t, &client, &set_content(9, items));
        assert!(
            !out.iter()
                .any(|e| matches!(e, BotEvent::SetWindowItems { .. }))
        );
        // player-region slot 27 → inventory window slot 9
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: 9,
                new: Some(item),
                ..
            } if item.kind() == ItemKind::Stone
        )));
        // nothing under the closed id
        assert!(
            !out.iter()
                .any(|e| matches!(e, BotEvent::WindowUpdateSlot { window_id: 9, .. }))
        );
    }
}
