//! Packet-to-event decode of the window packets, through the real engine
//! and the real bridge: the per-window-id events upstream's inventory.js
//! emits (`setSlot:{id}`, `setWindowItems:{id}`, `windowOpen`). Each step
//! keeps only the window events.

use mineflayer_engine::protocol::packets::game::{
    ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundOpenScreen,
};

use super::*;
use crate::test_util::sim::BotSim;

const CHEST_LEN: usize = 63;

fn step(
    sim: &mut BotSim,
    packet: impl mineflayer_engine::protocol::packets::Packet<ClientboundGamePacket>,
) -> Vec<BotEvent> {
    sim.receive(packet);
    sim.tick();
    sim.events()
        .into_iter()
        .filter(|e| {
            matches!(
                e,
                BotEvent::WindowUpdateSlot { .. }
                    | BotEvent::SetWindowItems { .. }
                    | BotEvent::HeldItemChanged(_)
                    | BotEvent::WindowOpen(_)
                    | BotEvent::WindowClose(_)
            )
        })
        .collect()
}

fn sim() -> BotSim {
    let mut sim = BotSim::new();
    sim.events();
    sim
}

fn open_screen(id: i32) -> ClientboundOpenScreen {
    ClientboundOpenScreen {
        container_id: id,
        menu_type: MenuKind::Generic9x3,
        title: FormattedText::from("chest"),
    }
}

fn set_content(id: i32, items: Vec<ItemStack>) -> ClientboundContainerSetContent {
    ClientboundContainerSetContent {
        container_id: id,
        state_id: 0,
        items,
        carried_item: ItemStack::Empty,
    }
}

fn set_slot(id: i32, slot: u16, stack: ItemStack) -> ClientboundContainerSetSlot {
    ClientboundContainerSetSlot {
        container_id: id,
        state_id: 0,
        slot,
        item_stack: stack,
    }
}

fn opened(events: &[BotEvent]) -> Window {
    events
        .iter()
        .find_map(|e| match e {
            BotEvent::WindowOpen(w) => Some(w.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no WindowOpen in {events:?}"))
}

/// Upstream `setSlot:{windowId}` — every resolved `set_slot` write
/// reports `(window_id, slot, old, new)`.
#[test]
fn set_slot_reports_per_window_slot_update() {
    let mut sim = sim();
    step(&mut sim, open_screen(3));
    step(&mut sim, set_content(3, vec![ItemStack::Empty; CHEST_LEN]));
    let out = step(&mut sim, set_slot(3, 2, ItemStack::new(ItemKind::Stone, 5)));
    assert!(out.iter().any(|e| matches!(
        e,
        BotEvent::WindowUpdateSlot {
            window_id: 3,
            slot: 2,
            old: None,
            new: Some(item),
        } if item.kind() == ItemKind::Stone && item.count() == 5
    )));
    // id-0/-2 writes report under window 0, with the replaced stack as old
    step(&mut sim, set_slot(0, 40, ItemStack::new(ItemKind::Dirt, 1)));
    let out = step(
        &mut sim,
        set_slot(-2, 40, ItemStack::new(ItemKind::Stone, 1)),
    );
    assert!(
        out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: 40,
                old: Some(old),
                new: Some(new),
            } if old.kind() == ItemKind::Dirt && new.kind() == ItemKind::Stone
        )),
        "{out:?}"
    );
}

/// Upstream `setWindowItems:{windowId}` — fires once per resolved
/// `container_set_content`, after the per-slot updates and
/// `heldItemChanged`, before `windowOpen` on first fill.
#[test]
fn set_content_emits_set_window_items_with_upstream_ordering() {
    let mut sim = sim();
    assert!(step(&mut sim, open_screen(3)).is_empty());
    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[0] = ItemStack::new(ItemKind::Stone, 1);
    let out = step(&mut sim, set_content(3, items));
    let pos = |pred: &dyn Fn(&BotEvent) -> bool| out.iter().position(pred).unwrap_or(usize::MAX);
    let last_slot = out
        .iter()
        .rposition(|e| matches!(e, BotEvent::WindowUpdateSlot { window_id: 3, .. }))
        .unwrap();
    let held = pos(&|e| matches!(e, BotEvent::HeldItemChanged(_)));
    let items_set = pos(&|e| matches!(e, BotEvent::SetWindowItems { window_id: 3 }));
    let opened = pos(&|e| matches!(e, BotEvent::WindowOpen(w) if w.id() == 3));
    assert!(
        last_slot < held && held < items_set && items_set < opened,
        "{out:?}"
    );
    assert_eq!(sim.bot.current_window().map(|w| w.id()), Some(3));

    // a resync of the still-open window emits it again, and its slot 0
    // update reports the first fill as old
    let out = step(&mut sim, set_content(3, vec![ItemStack::Empty; CHEST_LEN]));
    assert!(out.iter().any(|e| matches!(
        e,
        BotEvent::WindowUpdateSlot { window_id: 3, slot: 0, old: Some(old), new: None }
            if old.kind() == ItemKind::Stone
    )));
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::HeldItemChanged(_)))
    );
    assert!(matches!(
        out.last(),
        Some(BotEvent::SetWindowItems { window_id: 3 })
    ));

    // window 0 resolves too (upstream `bot.inventory`)
    let out = step(&mut sim, set_content(0, vec![ItemStack::Empty; 46]));
    assert!(matches!(
        out.last(),
        Some(BotEvent::SetWindowItems { window_id: 0 })
    ));
}

/// Slot data ahead of `open_screen` is buffered (upstream's
/// `windowItems`); the consuming `open_screen` emits `windowOpen` without
/// `setWindowItems` — upstream's prepareWindow sync branch emits only
/// `updateSlot` per item.
#[test]
fn buffered_items_open_without_set_window_items() {
    let mut sim = sim();
    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[1] = ItemStack::new(ItemKind::Stone, 3);
    assert!(step(&mut sim, set_content(7, items)).is_empty());
    let out = step(&mut sim, open_screen(7));
    assert!(
        !out.iter()
            .any(|e| matches!(e, BotEvent::SetWindowItems { .. }))
    );
    assert!(out.iter().any(|e| matches!(
        e,
        BotEvent::WindowUpdateSlot { window_id: 7, slot: 1, old: None, new: Some(item) }
            if item.count() == 3
    )));
    assert_eq!(opened(&out).id(), 7);
}

/// A `set_slot` between `open_screen` and the first `set_content` —
/// upstream's `currentWindow` exists already, so `setSlot:{id}` fires.
/// The fill's `WindowUpdateSlot` then reports that write as `old`.
#[test]
fn set_slot_to_pending_window_reports_update() {
    let mut sim = sim();
    assert!(step(&mut sim, open_screen(3)).is_empty());
    let out = step(&mut sim, set_slot(3, 2, ItemStack::new(ItemKind::Stone, 1)));
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
    assert!(
        sim.bot.current_window().is_none(),
        "no windowOpen before the fill"
    );

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[2] = ItemStack::new(ItemKind::Dirt, 4);
    let out = step(&mut sim, set_content(3, items));
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

/// Upstream buffers `windowItems` before the `lastClosedWindow` remap, so
/// a same-id reopen consumes the trailing sync as its contents.
#[test]
fn trailing_content_buffers_for_same_id_reopen() {
    let mut sim = sim();
    step(&mut sim, open_screen(9));
    let window = opened(&step(
        &mut sim,
        set_content(9, vec![ItemStack::Empty; CHEST_LEN]),
    ));
    window.close();
    sim.tick();

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[0] = ItemStack::new(ItemKind::Stone, 7);
    step(&mut sim, set_content(9, items));

    let out = step(&mut sim, open_screen(9));
    assert_eq!(opened(&out).id(), 9);
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

/// A trailing bulk sync under a client-closed id remaps its player region
/// into window 0 (upstream `lastClosedWindow`) and never emits
/// `setWindowItems` — upstream returns before the emit.
#[test]
fn trailing_content_remaps_to_inventory_without_set_window_items() {
    let mut sim = sim();
    step(&mut sim, open_screen(9));
    let window = opened(&step(
        &mut sim,
        set_content(9, vec![ItemStack::Empty; CHEST_LEN]),
    ));
    window.close();
    let out = sim.events();
    assert!(
        out.iter()
            .any(|e| matches!(e, BotEvent::WindowClose(w) if w.id() == 9))
    );
    sim.tick();

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[27] = ItemStack::new(ItemKind::Stone, 2);
    let out = step(&mut sim, set_content(9, items));
    assert!(
        !out.iter()
            .any(|e| matches!(e, BotEvent::SetWindowItems { .. }))
    );
    // player-region slot 27 → inventory window slot 9
    assert!(
        out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 0,
                slot: 9,
                old: None,
                new: Some(item),
            } if item.kind() == ItemKind::Stone
        )),
        "{out:?}"
    );
    assert!(
        !out.iter()
            .any(|e| matches!(e, BotEvent::WindowUpdateSlot { window_id: 9, .. }))
    );
    assert_eq!(
        sim.bot.inventory().slot(9).map(|i| i.kind()),
        Some(ItemKind::Stone)
    );

    // a single-slot trailing sync remaps the same way
    let out = step(&mut sim, set_slot(9, 28, ItemStack::new(ItemKind::Dirt, 1)));
    assert!(
        out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot { window_id: 0, slot: 10, new: Some(item), .. }
                if item.kind() == ItemKind::Dirt
        )),
        "{out:?}"
    );
}

/// After a login, a sync under the id of a container closed before it is
/// not a trailing sync any more: it is buffered, not remapped.
#[test]
fn login_ends_the_trailing_remap() {
    use mineflayer_engine::test_utils::simulation::default_login_packet;

    let mut sim = sim();
    step(&mut sim, open_screen(9));
    let window = opened(&step(
        &mut sim,
        set_content(9, vec![ItemStack::Empty; CHEST_LEN]),
    ));
    window.close();
    sim.tick();
    step(&mut sim, default_login_packet());

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[27] = ItemStack::new(ItemKind::Stone, 2);
    let out = step(&mut sim, set_content(9, items));
    assert!(
        !out.iter()
            .any(|e| matches!(e, BotEvent::WindowUpdateSlot { .. })),
        "{out:?}"
    );
}

/// Upstream applies a click to the window at once (`acceptClick`), so the
/// server's confirming `set_slot` reports the predicted stack as old, not
/// the stack before the click.
#[test]
fn a_click_confirmation_reports_the_predicted_stack_as_old() {
    use mineflayer_engine::inventory::operations::PickupClick;

    let mut sim = sim();
    step(&mut sim, open_screen(3));
    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[0] = ItemStack::new(ItemKind::Stone, 4);
    let window = opened(&step(&mut sim, set_content(3, items)));
    window.click(PickupClick::Left { slot: Some(0) });
    sim.tick();
    sim.events();

    let out = step(&mut sim, set_slot(3, 0, ItemStack::Empty));
    assert!(
        out.iter().any(|e| matches!(
            e,
            BotEvent::WindowUpdateSlot {
                window_id: 3,
                slot: 0,
                old: None,
                new: None
            }
        )),
        "{out:?}"
    );
}
