use std::{
    collections::VecDeque,
    sync::{Arc, atomic::Ordering},
};

use mineflayer_engine::{
    Event, FormattedText,
    core::entity_id::MinecraftEntityId,
    ecs::{
        self as bevy_ecs,
        prelude::{Component, Entity},
        world::World,
    },
    entity::inventory::Inventory,
    inventory::ItemStack,
    protocol::packets::game::ClientboundGamePacket,
    registry::builtin::{ItemKind, MenuKind},
};
use tokio::sync::{Notify, broadcast};

use crate::{BotEvent, bot::with_component_mut};

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

/// Inventory/window state of the bot, a component on the client entity —
/// the analog of upstream's `bot.currentWindow` plus the click/eat
/// bookkeeping in inventory.js. The window lifecycle the engine keeps
/// (the open id, early content, the last closed container, the slots) is
/// read from the engine `Inventory`, before each packet applies, through
/// [`WindowPreApply`].
#[derive(Component, Default)]
pub(crate) struct WindowTracker {
    /// The currently open non-inventory window (id != 0).
    pub current: Option<Arc<WindowShared>>,
    /// `open_screen` seen, waiting for first `container_set_content`.
    pub pending: Option<PendingWindow>,
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
}

impl WindowTracker {
    /// Held-item signature change → finish any in-flight eat and clear
    /// `using_held_item` (the upstream `heldItemChanged` listener only
    /// reacts to type/count changes).
    fn held_sig_changed(&mut self, inv: &Inventory) {
        let sig = held_signature(inv, self.selected_hotbar);
        if sig != self.last_held {
            self.last_held = sig;
            self.using_held_item = false;
            self.eat_notify.notify_waiters();
        }
    }

    /// The quickbar slot in hand: the tracked override, else the engine's.
    fn selected(&self, inv: &Inventory) -> u8 {
        self.selected_hotbar.unwrap_or(inv.selected_hotbar_slot)
    }

    /// Run the held-item check (upstream `updateHeldItem`) and report the
    /// held stack.
    fn held_changed(&mut self, inv: &Inventory) -> BotEvent {
        self.held_sig_changed(inv);
        BotEvent::HeldItemChanged(held_item_in(inv, self.selected(inv)))
    }

    /// The open window or the announced one has this id.
    fn resolves(&self, id: i32) -> bool {
        self.current.as_ref().is_some_and(|c| c.id == id)
            || self.pending.as_ref().is_some_and(|w| w.id == id)
    }
}

/// `f` on the bot's [`WindowTracker`] and the engine `Inventory`, in one
/// ECS access. `None` when the client entity or its `Inventory` is gone.
fn with_windows<R>(
    client: &mineflayer_engine::Client,
    f: impl FnOnce(&mut WindowTracker, &Inventory) -> R,
) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    // the tracker leaves its component for the call, so that `f` can read
    // the Inventory beside it
    let mut tracker = std::mem::take(&mut *entity.entry::<WindowTracker>().or_default().into_mut());
    let out = entity.get::<Inventory>().map(|inv| f(&mut tracker, inv));
    if let Some(mut slot) = entity.get_mut::<WindowTracker>() {
        *slot = tracker;
    }
    out
}

/// What one window packet is about to replace, read from the engine
/// `Inventory` before the engine applies the packet. The bridge runs
/// after the pass, when later packets may have changed the `Inventory`
/// again, so this is the only correct source of the `old` stacks and of
/// the lifecycle state each packet saw.
#[derive(Default)]
pub(crate) struct WindowPre {
    /// The packet's address inside its `Arc`, which the bridge gets in
    /// `Event::Packet`.
    packet: usize,
    /// `open_screen`: the early content for its id, which the open
    /// applies.
    early: Option<Vec<ItemStack>>,
    /// `(id, player_start, len)` of the container the client closed
    /// last (upstream `lastClosedWindow`).
    closed: Option<(i32, usize, usize)>,
    /// The slots of the menu the packet writes: all of them for
    /// `container_set_content`, one for a single-slot packet.
    old: Vec<ItemStack>,
}

/// The [`WindowPre`] records of the packets the bridge has not decoded
/// yet, in packet order.
#[derive(Component, Default)]
pub(crate) struct WindowPreApply(VecDeque<WindowPre>);

/// The `PacketPreApply` hook of [`super::WindowPlugin`].
pub(crate) fn record_window_pre(world: &mut World, entity: Entity, packet: &ClientboundGamePacket) {
    use ClientboundGamePacket as P;
    if !matches!(
        packet,
        P::OpenScreen(_)
            | P::ContainerSetContent(_)
            | P::ContainerSetSlot(_)
            | P::SetPlayerInventory(_)
    ) {
        return;
    }
    let Some(inv) = world.get::<Inventory>(entity) else {
        return;
    };
    let pre = WindowPre::read(inv, packet);
    match world.get_mut::<WindowPreApply>(entity) {
        Some(mut queue) => queue.0.push_back(pre),
        None => {
            world
                .entity_mut(entity)
                .insert(WindowPreApply(VecDeque::from([pre])));
        }
    }
}

impl WindowPre {
    /// The same routing as the engine's handlers
    /// (`container_set_content`, `container_set_slot`).
    fn read(inv: &Inventory, packet: &ClientboundGamePacket) -> Self {
        let closed = inv.last_closed;
        let closed_id = |id: i32| closed.is_some_and(|(closed, _, _)| closed == id);
        let one = |stack: Option<&ItemStack>| vec![stack.cloned().unwrap_or_default()];
        let (early, old) = match packet {
            ClientboundGamePacket::OpenScreen(p) => (
                inv.early_content
                    .as_ref()
                    .filter(|(id, _)| *id == p.container_id)
                    .map(|(_, items)| items.clone()),
                Vec::new(),
            ),
            ClientboundGamePacket::ContainerSetContent(p) => {
                let menu = if p.container_id == inv.id {
                    Some(inv.menu())
                } else if p.container_id == 0 || closed_id(p.container_id) {
                    Some(&inv.inventory_menu)
                } else {
                    None
                };
                (None, menu.map(|m| m.slots()).unwrap_or_default())
            }
            ClientboundGamePacket::ContainerSetSlot(p) => {
                let slot = usize::from(p.slot);
                let stack = match p.container_id {
                    -2 | 0 => inv.inventory_menu.slot(slot),
                    id if id == inv.id => inv.menu().slot(slot),
                    id if closed_id(id) => closed
                        .and_then(|(_, start, _)| slot.checked_sub(start))
                        .and_then(|i| inv.inventory_menu.slot(i + 9)),
                    _ => None,
                };
                (None, one(stack))
            }
            ClientboundGamePacket::SetPlayerInventory(p) => {
                let slot = mineflayer_engine::inventory::Player::slot_for_inventory_index(p.slot);
                (None, one(inv.inventory_menu.slot(slot)))
            }
            _ => (None, Vec::new()),
        };
        Self {
            packet: std::ptr::from_ref(packet) as usize,
            early,
            closed,
            old,
        }
    }

    /// The replaced stack at index `i` of [`Self::old`].
    fn old(&self, i: usize) -> &ItemStack {
        static EMPTY: ItemStack = ItemStack::Empty;
        self.old.get(i).unwrap_or(&EMPTY)
    }
}

/// The record of `packet`. Records of packets the bridge never decoded
/// are dropped on the way; with no record (a client with no engine
/// systems), every replaced stack is empty.
fn take_pre(client: &mineflayer_engine::Client, packet: &Arc<ClientboundGamePacket>) -> WindowPre {
    let address = Arc::as_ptr(packet) as usize;
    with_component_mut::<WindowPreApply, _>(client, |queue| {
        while let Some(pre) = queue.0.pop_front() {
            if pre.packet == address {
                return Some(pre);
            }
        }
        None
    })
    .flatten()
    .unwrap_or_default()
}

/// One `WindowUpdateSlot`.
fn slot_update(window_id: i32, slot: usize, old: &ItemStack, new: &ItemStack) -> BotEvent {
    BotEvent::WindowUpdateSlot {
        window_id,
        slot: slot as u16,
        old: Item::of(old.clone(), slot as i16),
        new: Item::of(new.clone(), slot as i16),
    }
}

/// `(kind, count)` of the held stack, `None` when the hand is empty.
/// `sel` overrides the engine's selection while
/// `set_selected_hotbar_slot`'s component write is still pending.
fn held_signature(inv: &Inventory, sel: Option<u8>) -> Option<(ItemKind, i32)> {
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

/// The stack in hotbar slot `sel` as an [`Item`] with its absolute window
/// slot, `None` when that slot is empty.
fn held_item_in(inv: &Inventory, sel: u8) -> Option<Item> {
    let index = crate::inventory::QUICK_BAR_START + usize::from(sel);
    Item::of(inv.inventory_menu.slot(index)?.clone(), index as i16)
}

/// [`held_item_in`] of the client's `Inventory`, `None` when it is gone.
pub(crate) fn held_item(client: &mineflayer_engine::Client, sel: u8) -> Option<Item> {
    held_item_in(&*client.get_component::<Inventory>()?, sel)
}

/// Decode one engine [`Event`] into window-domain [`BotEvent`]s — the
/// inventory-side analog of `entity::observe_event`. `events` is the bot's
/// event sender, which a new [`Window`] keeps for its `close`.
pub(crate) fn observe_event(
    client: &mineflayer_engine::Client,
    events: &broadcast::Sender<BotEvent>,
    event: &Event,
) -> Vec<BotEvent> {
    use ClientboundGamePacket as P;
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    // the records exist only for these packets; taking one for another
    // packet would drop the queue
    let pre = match &**packet {
        P::OpenScreen(_)
        | P::ContainerSetContent(_)
        | P::ContainerSetSlot(_)
        | P::SetPlayerInventory(_) => take_pre(client, packet),
        P::ContainerClose(_)
        | P::SetHeldSlot(_)
        | P::EntityEvent(_)
        | P::Cooldown(_)
        | P::Respawn(_)
        | P::Login(_) => WindowPre::default(),
        _ => return Vec::new(),
    };
    let self_id = match &**packet {
        P::EntityEvent(_) => client.get_component::<MinecraftEntityId>().map(|id| *id),
        _ => None,
    };
    let new_window = |id, kind, title: &FormattedText| {
        Window::new(id, kind, title.clone(), client.clone(), events.clone())
    };
    with_windows(client, |t, inv| match &**packet {
        P::OpenScreen(p) => {
            // a new screen displaces the previous open window without a
            // close packet — mark it closed like upstream replacing
            // `currentWindow`; an announced-but-unfilled window is
            // displaced the same way
            if let Some(current) = t.current.take() {
                current.closed.store(true, Ordering::SeqCst);
            }
            t.pending = None;
            if let Some(items) = pre.early {
                // slot data arrived before the open packet (upstream's
                // prepareWindow sync branch); the engine applied it to the
                // new, empty menu
                let window = new_window(p.container_id, p.menu_type, &p.title);
                t.current = Some(window.shared.clone());
                let mut out: Vec<BotEvent> = items
                    .iter()
                    .enumerate()
                    .map(|(i, stack)| slot_update(p.container_id, i, &ItemStack::Empty, stack))
                    .collect();
                out.push(t.held_changed(inv));
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
        P::ContainerSetContent(p) => set_content_events(t, inv, p, &pre, new_window),
        P::ContainerSetSlot(p) => set_slot_events(t, inv, p, &pre),
        P::ContainerClose(p) => {
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
            vec![BotEvent::WindowClose(Window { shared })]
        }
        P::SetHeldSlot(p) => {
            if p.slot > 8 {
                return Vec::new();
            }
            // upstream's held_item_slot handler only emits when the slot
            // actually changed
            let changed = u32::from(t.selected(inv)) != p.slot;
            t.selected_hotbar = Some(p.slot as u8);
            t.held_sig_changed(inv);
            if changed {
                vec![BotEvent::HeldItemChanged(held_item_in(inv, p.slot as u8))]
            } else {
                Vec::new()
            }
        }
        P::SetPlayerInventory(p) => {
            let win_slot = mineflayer_engine::inventory::Player::slot_for_inventory_index(p.slot);
            vec![
                slot_update(0, win_slot, pre.old(0), &p.contents),
                t.held_changed(inv),
            ]
        }
        P::EntityEvent(p) => {
            // upstream clears usingHeldItem on every entity_status packet
            t.using_held_item = false;
            // entity status 9 on our own entity = "finish using item"
            if p.event_id == 9 && self_id == Some(p.entity_id) {
                t.eat_notify.notify_waiters();
            }
            Vec::new()
        }
        P::Cooldown(p) => {
            // upstream finishes eating when the cooldown is for the held
            // item — or whenever heldItem is null
            if held_signature(inv, t.selected_hotbar).is_some_and(|(kind, _)| kind != p.item) {
                return Vec::new();
            }
            t.using_held_item = false;
            t.eat_notify.notify_waiters();
            Vec::new()
        }
        P::Respawn(_) | P::Login(_) => {
            // a new server-side player entity restarts the window id
            // counter, so an announced window is stale (the engine drops
            // its early content and last closed container too)
            t.pending = None;
            // the override only bridges set_selected_hotbar_slot's
            // tick-late component write — a fresh server-side player makes
            // any stale value wrong (upstream keeps `quickBarSlot`, and the
            // engine slot still reports it via the fallback path)
            t.selected_hotbar = None;
            let mut out = Vec::new();
            if matches!(&**packet, P::Login(_))
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
    })
    .unwrap_or_default()
}

/// `container_set_content` — full slot refresh for one window. Completes
/// a pending window into `current` + `windowOpen` (upstream's
/// `setWindowItems` gate), then emits one `WindowUpdateSlot` per reported
/// slot, like upstream calling `updateSlot` for each. Resolved packets
/// (window 0, a pending window's first fill, or the open container) end in
/// `SetWindowItems` — upstream's `setWindowItems:{id}`. A trailing sync
/// under the last closed container is remapped into window 0 and ends
/// before upstream's emit, so it gives no `SetWindowItems`; content for
/// any other id is buffered by the engine and gives no event.
fn set_content_events(
    t: &mut WindowTracker,
    inv: &Inventory,
    p: &mineflayer_engine::protocol::packets::game::c_container_set_content::ClientboundContainerSetContent,
    pre: &WindowPre,
    new_window: impl FnOnce(i32, MenuKind, &FormattedText) -> Window,
) -> Vec<BotEvent> {
    let id = p.container_id;
    let updates = |window_id: i32| -> Vec<BotEvent> {
        p.items
            .iter()
            .enumerate()
            .map(|(i, stack)| slot_update(window_id, i, pre.old(i), stack))
            .collect()
    };

    if id == 0 || t.current.as_ref().is_some_and(|c| c.id == id) {
        // upstream runs updateHeldItem(), then emits
        // `setWindowItems:{id}` — resolved packets only
        let mut out = updates(id);
        out.push(t.held_changed(inv));
        out.push(BotEvent::SetWindowItems { window_id: id });
        return out;
    }

    if let Some(pending) = t.pending.take_if(|w| w.id == id) {
        let window = new_window(pending.id, pending.menu_kind, &pending.title);
        t.current = Some(window.shared.clone());
        let mut out = updates(id);
        out.push(t.held_changed(inv));
        // upstream emits `setWindowItems:{id}` here; the once-listener
        // it registered in prepareWindow then fires `windowOpen`
        out.push(BotEvent::SetWindowItems { window_id: id });
        out.push(BotEvent::WindowOpen(window));
        return out;
    }

    if let Some((closed_id, player_start, len)) = pre.closed
        && id == closed_id
        && p.items.len() == len
    {
        // Trailing bulk sync under a window we closed client-side: its
        // player region still describes the player inventory — remap
        // into window 0 like upstream (the length check keeps an early
        // sync for a *different* same-id window from being misrouted).
        let mut out: Vec<BotEvent> = p
            .items
            .iter()
            .enumerate()
            .skip(player_start)
            .map(|(i, stack)| {
                let inv_slot = i - player_start + 9;
                slot_update(0, inv_slot, pre.old(inv_slot), stack)
            })
            .collect();
        // each remapped write goes through upstream's `_setSlot`, which
        // runs updateHeldItem() — once per packet here
        out.push(t.held_changed(inv));
        return out;
    }
    Vec::new()
}

/// `container_set_slot` — single-slot write. `container_id` -1 is the
/// cursor (applied by the engine, no event upstream), -2 and 0 hit the player
/// window, anything else resolves the open *or pending* container (or a
/// trailing sync under the last closed container) — upstream's
/// `currentWindow` is set at `open_window`, before its first
/// `window_items` arrives.
fn set_slot_events(
    t: &mut WindowTracker,
    inv: &Inventory,
    p: &mineflayer_engine::protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot,
    pre: &WindowPre,
) -> Vec<BotEvent> {
    let slot = usize::from(p.slot);
    let window_slot = match p.container_id {
        -1 => {
            // cursor item — the engine applies it; upstream emits nothing.
            t.held_sig_changed(inv);
            return Vec::new();
        }
        -2 | 0 => Some((0, slot)),
        id if t.resolves(id) => Some((id, slot)),
        // Trailing sync under a window we closed client-side: its player
        // region still describes the player inventory — remap into
        // window 0 like upstream.
        id => pre
            .closed
            .filter(|&(closed, start, _)| closed == id && (start..start + 36).contains(&slot))
            .map(|(_, start, _)| (0, slot - start + 9)),
    };
    let Some((window_id, slot)) = window_slot else {
        return Vec::new();
    };
    // upstream's `_setSlot` runs updateHeldItem() after each write — a
    // remapped hotbar write can change the held stack too
    vec![
        slot_update(window_id, slot, pre.old(0), &p.item_stack),
        t.held_changed(inv),
    ]
}

#[cfg(test)]
mod tests;
