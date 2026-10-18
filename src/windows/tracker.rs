use std::{
    collections::VecDeque,
    sync::{Arc, atomic::Ordering},
};

use parking_lot::Mutex;

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

/// Inventory/window state shared between `Bot` methods and the bridge —
/// the analog of upstream's `bot.currentWindow` plus the click/eat
/// bookkeeping in inventory.js. The window lifecycle the engine keeps
/// (the open id, early content, the last closed container, the slots) is
/// read from the engine `Inventory`, before each packet applies, through
/// [`WindowPreApply`].
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
    /// Bot event sender — `Window::close` emits `WindowClose` itself.
    pub events: broadcast::Sender<BotEvent>,
}

impl WindowTracker {
    pub(crate) fn new(events: broadcast::Sender<BotEvent>) -> Self {
        Self {
            current: None,
            pending: None,
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
    /// lock rule: called with the tracker lock held; the ECS read below
    /// is brief (domain lock → ECS lock is the allowed direction).
    fn held_sig_changed(&mut self, client: &mineflayer_engine::Client) {
        let sig = held_signature(client, self.selected_hotbar);
        if sig != self.last_held {
            self.last_held = sig;
            self.using_held_item = false;
            self.eat_notify.notify_waiters();
        }
    }

    /// The quickbar slot in hand: the tracked override, else the ECS
    /// value, else 0.
    fn selected(&self, client: &mineflayer_engine::Client) -> u8 {
        self.selected_hotbar.unwrap_or_else(|| {
            client
                .get_component::<Inventory>()
                .map(|i| i.selected_hotbar_slot)
                .unwrap_or(0)
        })
    }

    /// Run the held-item check (upstream `updateHeldItem`) and report the
    /// held stack.
    fn held_changed(&mut self, client: &mineflayer_engine::Client) -> BotEvent {
        self.held_sig_changed(client);
        BotEvent::HeldItemChanged(held_item(client, self.selected(client)))
    }

    /// The open window or the announced one has this id.
    fn resolves(&self, id: i32) -> bool {
        self.current.as_ref().is_some_and(|c| c.id == id)
            || self.pending.as_ref().is_some_and(|w| w.id == id)
    }
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

/// `(kind, count)` of the currently held stack, `None` when empty or the
/// component is missing (pre-login). `sel` overrides the ECS selection
/// while `set_selected_hotbar_slot`'s component write is still pending.
///
/// lock rule: callers hold the tracker lock; the ECS read is brief
/// (domain lock → ECS lock is the allowed direction).
fn held_signature(client: &mineflayer_engine::Client, sel: Option<u8>) -> Option<(ItemKind, i32)> {
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
/// lock rule: callers hold the tracker lock; the ECS read is brief
/// (domain lock → ECS lock is the allowed direction).
pub(crate) fn held_item(client: &mineflayer_engine::Client, sel: u8) -> Option<Item> {
    let inv = client.get_component::<Inventory>()?;
    let index = crate::inventory::QUICK_BAR_START + usize::from(sel);
    Item::of(inv.inventory_menu.slot(index)?.clone(), index as i16)
}

/// Decode one engine [`Event`] into window-domain [`BotEvent`]s — the
/// inventory-side analog of `entity::observe_event`.
pub(crate) fn observe_event(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &mineflayer_engine::Client,
    event: &Event,
) -> Vec<BotEvent> {
    let Event::Packet(packet) = event else {
        return Vec::new();
    };
    match &**packet {
        ClientboundGamePacket::OpenScreen(p) => {
            let pre = take_pre(client, packet);
            let mut t = tracker.lock();
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
                let window = Window::new(
                    p.container_id,
                    p.menu_type,
                    p.title.clone(),
                    client.clone(),
                    tracker,
                );
                t.current = Some(window.shared.clone());
                let mut out: Vec<BotEvent> = items
                    .iter()
                    .enumerate()
                    .map(|(i, stack)| slot_update(p.container_id, i, &ItemStack::Empty, stack))
                    .collect();
                out.push(t.held_changed(client));
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
        ClientboundGamePacket::ContainerSetContent(p) => {
            let pre = take_pre(client, packet);
            set_content_events(tracker, client, p, &pre)
        }
        ClientboundGamePacket::ContainerSetSlot(p) => {
            let pre = take_pre(client, packet);
            set_slot_events(tracker, client, p, &pre)
        }
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
            vec![BotEvent::WindowClose(Window { shared })]
        }
        ClientboundGamePacket::SetHeldSlot(p) => {
            if p.slot > 8 {
                return Vec::new();
            }
            // upstream's held_item_slot handler only emits when the slot
            // actually changed
            // lock rule: tracker lock held while `held_sig_changed` /
            // `held_item` take brief ECS reads — allowed direction.
            let mut t = tracker.lock();
            let changed = u32::from(t.selected(client)) != p.slot;
            t.selected_hotbar = Some(p.slot as u8);
            t.held_sig_changed(client);
            if changed {
                vec![BotEvent::HeldItemChanged(held_item(client, p.slot as u8))]
            } else {
                Vec::new()
            }
        }
        ClientboundGamePacket::SetPlayerInventory(p) => {
            let pre = take_pre(client, packet);
            let win_slot = mineflayer_engine::inventory::Player::slot_for_inventory_index(p.slot);
            let mut t = tracker.lock();
            vec![
                slot_update(0, win_slot, pre.old(0), &p.contents),
                t.held_changed(client),
            ]
        }
        ClientboundGamePacket::EntityEvent(p) => {
            // lock rule: tracker lock held while `get_component` takes a
            // brief ECS read — allowed direction.
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
            // lock rule: tracker lock held across `held_signature`'s ECS
            // read — allowed direction.
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
            // counter, so an announced window is stale (the engine drops
            // its early content and last closed container too)
            let mut t = tracker.lock();
            t.pending = None;
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
/// (window 0, a pending window's first fill, or the open container) end in
/// `SetWindowItems` — upstream's `setWindowItems:{id}`. A trailing sync
/// under the last closed container is remapped into window 0 and ends
/// before upstream's emit, so it gives no `SetWindowItems`; content for
/// any other id is buffered by the engine and gives no event.
fn set_content_events(
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &mineflayer_engine::Client,
    p: &mineflayer_engine::protocol::packets::game::c_container_set_content::ClientboundContainerSetContent,
    pre: &WindowPre,
) -> Vec<BotEvent> {
    // lock rule: tracker lock held while ECS reads (`get_component`,
    // `held_sig_changed`, `held_item`) run — allowed direction.
    let mut t = tracker.lock();
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
        out.push(t.held_changed(client));
        out.push(BotEvent::SetWindowItems { window_id: id });
        return out;
    }

    if let Some(pending) = t.pending.take_if(|w| w.id == id) {
        let window = Window::new(
            pending.id,
            pending.menu_kind,
            pending.title,
            client.clone(),
            tracker,
        );
        t.current = Some(window.shared.clone());
        let mut out = updates(id);
        out.push(t.held_changed(client));
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
        out.push(t.held_changed(client));
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
    tracker: &Arc<Mutex<WindowTracker>>,
    client: &mineflayer_engine::Client,
    p: &mineflayer_engine::protocol::packets::game::c_container_set_slot::ClientboundContainerSetSlot,
    pre: &WindowPre,
) -> Vec<BotEvent> {
    // lock rule: the tracker lock is held while brief ECS reads are taken
    // — allowed direction.
    let mut t = tracker.lock();
    let slot = usize::from(p.slot);
    let window_slot = match p.container_id {
        -1 => {
            // cursor item — the engine applies it; upstream emits nothing.
            t.held_sig_changed(client);
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
        t.held_changed(client),
    ]
}

#[cfg(test)]
mod tests;
