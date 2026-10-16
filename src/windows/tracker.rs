use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
};

use parking_lot::Mutex;

use azalea::{
    Event, FormattedText,
    core::entity_id::MinecraftEntityId,
    entity::inventory::Inventory,
    inventory::ItemStack,
    protocol::packets::game::ClientboundGamePacket,
    registry::builtin::{ItemKind, MenuKind},
};
use tokio::sync::{Notify, broadcast};

use crate::BotEvent;

use super::{Item, Window, WindowShared};

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
            let mut t = tracker.lock();
            // a new screen displaces the previous open window without a
            // close packet — mark it closed like upstream replacing
            // `currentWindow`, and clear `lastClosedWindow` (upstream's
            // open_window handler does).
            if let Some(current) = t.current.take() {
                current.closed.store(true, Ordering::SeqCst);
                t.slots.remove(&current.id);
            }
            t.last_closed = None;
            // an announced-but-unfilled window is displaced the same way —
            // upstream's currentWindow replacement drops it
            t.pending = None;
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
            let mut t = tracker.lock();
            // the server may close a pending window before content ever
            // arrived — drop it only if the close names the same id
            t.pending.take_if(|w| w.id == p.container_id);
            let Some(shared) = t.current.take() else {
                // server acking our own close() — already emitted.
                // upstream emits windowClose with `null` here; a typed
                // `Window` payload can't express null, so the event is
                // skipped — documented deviation in api-parity.md.
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
            let mut t = tracker.lock();
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
            let mut t = tracker.lock();
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
            let mut t = tracker.lock();
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
            let mut t = tracker.lock();
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
            let mut t = tracker.lock();
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
/// (window 0, a pending window's first fill, or the open container) get
/// the `carried_item` patch and end in `SetWindowItems` — upstream's
/// `setWindowItems:{id}`. Buffered (`early_items`) and `last_closed`
/// remaps return before upstream's emit, so they produce no
/// `SetWindowItems` — a remap still buffers the packet for a same-id
/// reopen and runs the held-item check, like upstream's `windowItems`
/// ordering.
fn set_content_events(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &azalea::Client,
    p: &azalea::protocol::packets::game::c_container_set_content::ClientboundContainerSetContent,
) -> Vec<BotEvent> {
    let mut t = tracker.lock();
    let sel = t.selected_hotbar.unwrap_or_else(|| {
        client
            .get_component::<Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    });
    let mut out = Vec::new();

    // Adopt when this packet resolves the active menu (see patch_state_id).
    if p.container_id == active_inventory_id(client) {
        patch_state_id(client, p.state_id);
    }

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
        // upstream applies carriedItem, then updateHeldItem(), then
        // emits `setWindowItems:{id}` — resolved packets only
        patch_carried(client, &p.carried_item);
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
        patch_carried(client, &p.carried_item);
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
        patch_carried(client, &p.carried_item);
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
        // Upstream buffers `windowItems = packet` before remapping, so a
        // same-id reopen consumes these contents.
        t.early_items = Some((p.container_id, p.items.clone()));
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
        // each remapped write goes through upstream's `_setSlot`, which
        // runs updateHeldItem() — once per packet here
        t.held_sig_changed(client);
        out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
        return out;
    }

    // content for a window id we've never seen — buffer it; a matching
    // `open_screen` consumes it (upstream `windowItems`).
    t.early_items = Some((p.container_id, p.items.clone()));
    out
}

/// Write `container_set_content`'s `carried_item` into the ECS — azalea
/// ignores the field, so without this the cursor stack desyncs and click
/// packets carry the wrong item. Upstream applies it only on resolved
/// packets (window 0 or `currentWindow`, which includes pending windows);
/// early-buffered and `lastClosedWindow`-remapped packets return first.
fn patch_carried(client: &azalea::Client, stack: &ItemStack) {
    let mut ecs = client.ecs.write();
    if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity) {
        inv.carried = stack.clone();
    }
}

/// Protocol id of the menu clicks target — azalea's `Inventory.id`
/// (0 = the player window, which `WindowTracker` mirrors in `pending`/
/// `current`).
fn active_inventory_id(client: &azalea::Client) -> i32 {
    client
        .get_component::<Inventory>()
        .map(|inv| inv.id)
        .unwrap_or(0)
}

/// Adopt a packet's `state_id` for the active menu — vanilla's
/// `initializeContents` / `setItem(slot, stateId, item)` update the active
/// menu's id, and upstream mineflayer mirrors it with a global
/// `stateId = packet.stateId` listener over `set_slot`/`window_items`.
/// azalea adopts one only from non-hotbar `set_slot` writes: hotbar writes
/// into an idle window 0 skip it and `container_set_content` never carries
/// it, so after a pickup or a full resync our clicks go out stale. The
/// server answers a stale id with a full resync instead of a slot diff,
/// and a resync landing mid-choreography (craft -> grab -> deposit)
/// clobbers the cursor the next click is gated on.
fn patch_state_id(client: &azalea::Client, state_id: u32) {
    let mut ecs = client.ecs.write();
    if let Some(mut inv) = ecs.get_mut::<Inventory>(client.entity) {
        inv.state_id = state_id;
    }
}

/// `container_set_slot` — single-slot write. `container_id` -1 is the
/// cursor (applied by azalea, no event upstream), -2 and 0 hit the player
/// window, anything else resolves the open *or pending* container (or a
/// trailing sync under `last_closed`) — upstream's `currentWindow` is set
/// at `open_window`, before its first `window_items` arrives.
fn set_slot_events(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &azalea::Client,
    p: &azalea::protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot,
) -> Vec<BotEvent> {
    let mut t = tracker.lock();
    let sel = t.selected_hotbar.unwrap_or_else(|| {
        client
            .get_component::<Inventory>()
            .map(|i| i.selected_hotbar_slot)
            .unwrap_or(0)
    });
    let mut out = Vec::new();

    // Adopt when this packet resolves the active menu — hotbar writes into
    // an idle window 0 included (see patch_state_id).
    if p.container_id == active_inventory_id(client) {
        patch_state_id(client, p.state_id);
    }

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
            // `pending` resolves too — upstream's `currentWindow` exists
            // from `open_window`, so a set_slot that lands before the
            // first fill still emits `setSlot:{id}` (and its snapshot
            // entry becomes the fill's `old`).
            if t.current.as_ref().is_some_and(|c| c.id == id)
                || t.pending.as_ref().is_some_and(|w| w.id == id)
            {
                let old = snapshot_set(&mut t, id, p.slot as usize, p.item_stack.clone());
                out.push(BotEvent::WindowUpdateSlot {
                    window_id: id,
                    slot: p.slot,
                    old,
                    new: Item::of(p.item_stack.clone(), p.slot as i16),
                });
                t.held_sig_changed(client);
                out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
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
                // upstream's `_setSlot` runs updateHeldItem() here too —
                // a remapped hotbar write can change the held stack
                t.held_sig_changed(client);
                out.push(BotEvent::HeldItemChanged(held_item(client, sel)));
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
    use azalea::inventory::Menu;

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

    fn client_with(inv: Inventory) -> azalea::Client {
        crate::test_util::live_entity_client(inv)
    }

    fn state_id_of(client: &azalea::Client) -> u32 {
        client.get_component::<Inventory>().unwrap().state_id
    }

    fn open_screen(id: i32) -> Event {
        packet_event(ClientboundGamePacket::OpenScreen(ClientboundOpenScreen {
            container_id: id,
            menu_type: MenuKind::Generic9x3,
            title: azalea::FormattedText::from("chest"),
        }))
    }

    fn set_content(id: i32, items: Vec<ItemStack>) -> Event {
        set_content_sid(id, 0, items)
    }

    fn set_content_sid(id: i32, state_id: u32, items: Vec<ItemStack>) -> Event {
        packet_event(ClientboundGamePacket::ContainerSetContent(
            ClientboundContainerSetContent {
                container_id: id,
                state_id,
                items,
                carried_item: ItemStack::Empty,
            },
        ))
    }

    fn set_slot(id: i32, slot: u16, stack: ItemStack) -> Event {
        set_slot_sid(id, 0, slot, stack)
    }

    fn set_slot_sid(id: i32, state_id: u32, slot: u16, stack: ItemStack) -> Event {
        packet_event(ClientboundGamePacket::ContainerSetSlot(
            ClientboundContainerSetSlot {
                container_id: id,
                state_id,
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

    /// The live-craft regression: clicks carried a stale `state_id`
    /// because azalea adopts one only from non-hotbar `set_slot` writes —
    /// never from `container_set_content`, and never from hotbar writes.
    /// The server answers a stale id with a full resync instead of a slot
    /// diff, and a resync landing mid-choreography (craft -> grab ->
    /// deposit) clobbers the cursor the next click is gated on. Vanilla
    /// adopts the id from packets resolving the active menu
    /// (`initializeContents` / `setItem(slot, stateId, item)`); upstream
    /// mineflayer tracks it globally from every `set_slot`/`window_items`.
    #[test]
    fn state_id_adopts_from_active_menu_packets() {
        let t = tracker();
        let client = client();

        // hotbar set_slot into an idle window 0 adopts (azalea skips these)
        observe_event(
            &t,
            &client,
            &set_slot_sid(0, 9, 36, ItemStack::new(ItemKind::OakLog, 1)),
        );
        assert_eq!(state_id_of(&client), 9);

        // resolved window-0 bulk content adopts (azalea never does)
        observe_event(
            &t,
            &client,
            &set_content_sid(0, 12, vec![ItemStack::Empty; 46]),
        );
        assert_eq!(state_id_of(&client), 12);

        // an unrelated window's bulk content buffers without adopting
        observe_event(
            &t,
            &client,
            &set_content_sid(7, 40, vec![ItemStack::Empty; 63]),
        );
        assert_eq!(state_id_of(&client), 12);

        // cursor packets (`-1`) never initialize a menu — no adoption
        observe_event(
            &t,
            &client,
            &set_slot_sid(-1, 77, 0, ItemStack::new(ItemKind::OakLog, 1)),
        );
        assert_eq!(state_id_of(&client), 12);
    }

    /// While a container is open, only its own packets move the tracked
    /// id — the id-0 hotbar specials and window-0 bulk resyncs must not
    /// (vanilla gates adoption on the active menu).
    #[test]
    fn state_id_tracks_the_open_container() {
        let t = tracker();
        let inv = Inventory {
            id: 5,
            container_menu: Some(Menu::from_kind(MenuKind::Generic9x3)),
            ..Inventory::default()
        };
        let client = client_with(inv);
        observe_event(&t, &client, &open_screen(5));

        // the open container's set_slot and bulk content adopt
        observe_event(
            &t,
            &client,
            &set_slot_sid(5, 4, 2, ItemStack::new(ItemKind::Stone, 1)),
        );
        assert_eq!(state_id_of(&client), 4);
        observe_event(
            &t,
            &client,
            &set_content_sid(5, 3, vec![ItemStack::Empty; 63]),
        );
        assert_eq!(state_id_of(&client), 3);

        // an id-0 hotbar write while the container is open does not
        observe_event(
            &t,
            &client,
            &set_slot_sid(0, 9, 36, ItemStack::new(ItemKind::OakLog, 1)),
        );
        assert_eq!(state_id_of(&client), 3);

        // nor does an id-0 bulk resync of the player window
        observe_event(
            &t,
            &client,
            &set_content_sid(0, 21, vec![ItemStack::Empty; 46]),
        );
        assert_eq!(state_id_of(&client), 3);
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

    /// A `set_slot` between `open_screen` and the first `set_content` —
    /// upstream's `currentWindow` exists already, so `setSlot:{id}`
    /// fires. The write is also in the slot snapshot, so the fill's
    /// `WindowUpdateSlot` reports it as `old` rather than `None`.
    #[test]
    fn set_slot_to_pending_window_reports_update() {
        let t = tracker();
        let client = client();
        assert!(observe_event(&t, &client, &open_screen(3)).is_empty());
        let out = observe_event(
            &t,
            &client,
            &set_slot(3, 2, ItemStack::new(ItemKind::Stone, 1)),
        );
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 3,
                slot: 2,
                old: None,
                new: Some(item),
            } if item.kind() == ItemKind::Stone
        )));
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::HeldItemChanged(_)))
        );

        // the first fill's slot-2 update reports the pending write as old
        let mut items = vec![ItemStack::Empty; 63];
        items[2] = ItemStack::new(ItemKind::Dirt, 4);
        let out = observe_event(&t, &client, &set_content(3, items));
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 3,
                slot: 2,
                old: Some(old),
                new: Some(new),
            } if old.kind() == ItemKind::Stone && new.kind() == ItemKind::Dirt
        )));
        assert!(
            out.iter()
                .any(|e| matches!(e, BotEvent::SetWindowItems { window_id: 3 }))
        );
    }

    /// `carried_item` applies only on resolved packets — upstream sets
    /// `selectedItem` after the unresolved-path early return, so a stale
    /// trailing sync must not overwrite the cursor.
    #[test]
    fn carried_item_applies_only_on_resolved_set_content() {
        let t = tracker();
        let client = client();
        let carried =
            |client: &azalea::Client| client.get_component::<Inventory>().unwrap().carried.clone();

        // buffered packet for an unseen id — carried untouched
        let stone = ItemStack::new(ItemKind::Stone, 1);
        observe_event(
            &t,
            &client,
            &packet_event(ClientboundGamePacket::ContainerSetContent(
                ClientboundContainerSetContent {
                    container_id: 7,
                    state_id: 0,
                    items: vec![ItemStack::Empty; 63],
                    carried_item: stone.clone(),
                },
            )),
        );
        assert!(carried(&client).is_empty());

        // a resolved packet applies it
        observe_event(
            &t,
            &client,
            &packet_event(ClientboundGamePacket::ContainerSetContent(
                ClientboundContainerSetContent {
                    container_id: 0,
                    state_id: 0,
                    items: vec![ItemStack::Empty; 46],
                    carried_item: stone.clone(),
                },
            )),
        );
        assert_eq!(carried(&client), stone);
    }

    /// Upstream buffers `windowItems` before the `lastClosedWindow`
    /// remap, so a same-id reopen consumes the trailing sync as its
    /// contents.
    #[test]
    fn trailing_content_buffers_for_same_id_reopen() {
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

        let mut items = vec![ItemStack::Empty; 63];
        items[0] = ItemStack::new(ItemKind::Stone, 7);
        observe_event(&t, &client, &set_content(9, items));

        // reopening under the same id consumes the buffered contents
        let out = observe_event(&t, &client, &open_screen(9));
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowOpen(w) if w.id() == 9
        )));
        assert!(out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 9,
                slot: 0,
                new: Some(item),
                ..
            } if item.kind() == ItemKind::Stone && item.count() == 7
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
        assert_eq!(t.lock().last_closed, Some((9, 27, 63)));

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
