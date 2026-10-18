//! Inventory packets the client applies to its `Inventory` component.
//!
//! Each case names the vanilla client's behavior (`ClientPacketListener`)
//! and upstream mineflayer's (`lib/plugins/inventory.js`) where they differ.

use mineflayer_chat::FormattedText;
use mineflayer_client::{inventory::CloseContainerEvent, test_utils::prelude::*};
use mineflayer_entity::inventory::Inventory;
use mineflayer_inventory::ItemStack;
use mineflayer_protocol::packets::{
    ConnectionProtocol,
    game::{
        ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundOpenScreen,
        ClientboundSetCursorItem, ClientboundSetPlayerInventory,
    },
};
use mineflayer_registry::builtin::{ItemKind, MenuKind};

const PLAYER_MENU_LEN: usize = 46;
const CHEST_LEN: usize = 63;
const CHEST_PLAYER_START: usize = 27;

fn logged_in() -> Simulation {
    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    simulation.receive_packet(default_login_packet());
    simulation.tick();
    simulation
}

fn open_chest(simulation: &mut Simulation, id: i32) {
    simulation.receive_packet(ClientboundOpenScreen {
        container_id: id,
        menu_type: MenuKind::Generic9x3,
        title: FormattedText::default(),
    });
    simulation.tick();
}

fn content(
    id: i32,
    state_id: u32,
    items: Vec<ItemStack>,
    carried: ItemStack,
) -> ClientboundContainerSetContent {
    ClientboundContainerSetContent {
        container_id: id,
        state_id,
        items,
        carried_item: carried,
    }
}

fn slot(id: i32, state_id: u32, slot: u16, item: ItemStack) -> ClientboundContainerSetSlot {
    ClientboundContainerSetSlot {
        container_id: id,
        state_id,
        slot,
        item_stack: item,
    }
}

fn stone(count: i32) -> ItemStack {
    ItemStack::new(ItemKind::Stone, count)
}

fn inventory(simulation: &Simulation) -> Inventory {
    simulation.component::<Inventory>()
}

/// Vanilla `initializeContents(stateId, items, carried)` on the active
/// menu sets the cursor and the state id along with the slots.
#[test]
fn set_content_for_the_active_menu_sets_carried_and_state_id() {
    let _lock = init();
    let mut simulation = logged_in();

    simulation.receive_packet(content(
        0,
        12,
        vec![ItemStack::Empty; PLAYER_MENU_LEN],
        stone(1),
    ));
    simulation.tick();
    let inv = inventory(&simulation);
    assert_eq!(inv.carried, stone(1));
    assert_eq!(inv.state_id, 12);

    open_chest(&mut simulation, 5);
    simulation.receive_packet(content(5, 3, vec![ItemStack::Empty; CHEST_LEN], stone(2)));
    simulation.tick();
    let inv = inventory(&simulation);
    assert_eq!(inv.carried, stone(2));
    assert_eq!(inv.state_id, 3);

    // id 0 while the chest is open initializes the inactive player menu
    // in vanilla; its cursor and state id are not the ones clicks use.
    // Upstream mineflayer sets the cursor here too; we follow vanilla.
    simulation.receive_packet(content(
        0,
        21,
        vec![ItemStack::Empty; PLAYER_MENU_LEN],
        stone(9),
    ));
    simulation.tick();
    let inv = inventory(&simulation);
    assert_eq!(inv.carried, stone(2));
    assert_eq!(inv.state_id, 3);
}

/// Vanilla `setItem(slot, stateId, item)` adopts the state id whenever
/// the write lands on the active menu, hotbar writes into window 0
/// included. Cursor writes (`-1`) never carry one.
#[test]
fn set_slot_on_the_active_menu_adopts_state_id() {
    let _lock = init();
    let mut simulation = logged_in();

    simulation.receive_packet(slot(0, 9, 36, stone(1)));
    simulation.tick();
    assert_eq!(inventory(&simulation).state_id, 9);

    simulation.receive_packet(slot(-1, 77, 0, stone(1)));
    simulation.tick();
    assert_eq!(inventory(&simulation).state_id, 9);

    open_chest(&mut simulation, 5);
    simulation.receive_packet(slot(5, 4, 2, stone(1)));
    simulation.tick();
    assert_eq!(inventory(&simulation).state_id, 4);

    // a hotbar write into window 0 while the chest is open sets the
    // inactive player menu's state id in vanilla, not the chest's
    simulation.receive_packet(slot(0, 30, 36, stone(3)));
    simulation.tick();
    let inv = inventory(&simulation);
    assert_eq!(inv.state_id, 4);
    assert_eq!(inv.inventory_menu.slot(36), Some(&stone(3)));
}

/// Vanilla drops a window-0 `set_slot` for a slot outside the hotbar
/// while a container is open (its menu id is not active). Upstream
/// mineflayer applies every window-0 write to `bot.inventory`, and the
/// server already counts the write as delivered, so we apply it.
#[test]
fn window_zero_set_slot_outside_the_hotbar_applies_while_a_container_is_open() {
    let _lock = init();
    let mut simulation = logged_in();
    open_chest(&mut simulation, 5);

    simulation.receive_packet(slot(0, 0, 10, stone(4)));
    simulation.tick();
    assert_eq!(
        inventory(&simulation).inventory_menu.slot(10),
        Some(&stone(4))
    );
}

/// `set_player_inventory` addresses vanilla `Inventory` indices; vanilla
/// applies it with `player.getInventory().setItem`. Upstream maps it with
/// `playerInventorySlotToWindow`.
#[test]
fn set_player_inventory_writes_the_mapped_player_menu_slot() {
    let _lock = init();
    let mut simulation = logged_in();

    for (inventory_index, window_slot) in [
        (0, 36),
        (8, 44),
        (9, 9),
        (35, 35),
        (36, 8),
        (39, 5),
        (40, 45),
    ] {
        simulation.receive_packet(ClientboundSetPlayerInventory {
            slot: inventory_index,
            contents: stone(inventory_index as i32 + 1),
        });
        simulation.tick();
        assert_eq!(
            inventory(&simulation).inventory_menu.slot(window_slot),
            Some(&stone(inventory_index as i32 + 1)),
            "inventory index {inventory_index}"
        );
    }
}

/// Vanilla `handleSetCursorItem` sets the active menu's cursor.
#[test]
fn set_cursor_item_sets_carried() {
    let _lock = init();
    let mut simulation = logged_in();

    simulation.receive_packet(ClientboundSetCursorItem { contents: stone(6) });
    simulation.tick();
    assert_eq!(inventory(&simulation).carried, stone(6));
}

/// Vanilla drops content for a menu id that is not open. Upstream
/// mineflayer buffers it (`windowItems`) and applies it when the
/// matching `open_window` arrives; we follow upstream.
#[test]
fn content_before_open_screen_fills_the_menu_when_it_opens() {
    let _lock = init();
    let mut simulation = logged_in();

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[0] = stone(7);
    simulation.receive_packet(content(7, 2, items, stone(1)));
    simulation.tick();
    // a buffered packet is not resolved: the cursor stays
    assert!(inventory(&simulation).carried.is_empty());

    open_chest(&mut simulation, 7);
    let inv = inventory(&simulation);
    assert_eq!(
        inv.container_menu.as_ref().unwrap().slot(0),
        Some(&stone(7))
    );
    assert!(inv.carried.is_empty());
}

/// After the client closes a container, the server can still send
/// syncs under the closed id. Vanilla drops them. Upstream mineflayer
/// (`lastClosedWindow`) applies their player region to the player
/// inventory; we follow upstream.
#[test]
fn trailing_syncs_under_a_client_closed_id_reach_the_player_menu() {
    let _lock = init();
    let mut simulation = logged_in();
    open_chest(&mut simulation, 9);
    simulation.receive_packet(content(
        9,
        1,
        vec![ItemStack::Empty; CHEST_LEN],
        ItemStack::Empty,
    ));
    simulation.tick();

    simulation.trigger(CloseContainerEvent {
        entity: simulation.entity,
        id: 9,
    });
    simulation.tick();
    assert_eq!(inventory(&simulation).id, 0);

    let mut items = vec![ItemStack::Empty; CHEST_LEN];
    items[CHEST_PLAYER_START] = stone(2);
    simulation.receive_packet(content(9, 8, items, stone(5)));
    simulation.tick();
    let inv = inventory(&simulation);
    // player-region slot 27 of the chest is player-menu slot 9
    assert_eq!(inv.inventory_menu.slot(9), Some(&stone(2)));
    assert!(inv.carried.is_empty());

    simulation.receive_packet(slot(9, 0, (CHEST_PLAYER_START + 1) as u16, stone(3)));
    simulation.tick();
    assert_eq!(
        inventory(&simulation).inventory_menu.slot(10),
        Some(&stone(3))
    );

    // a server-side close does not arm the remap
    open_chest(&mut simulation, 10);
    simulation.receive_packet(
        mineflayer_protocol::packets::game::ClientboundContainerClose { container_id: 10 },
    );
    simulation.tick();
    simulation.receive_packet(slot(10, 0, CHEST_PLAYER_START as u16, stone(8)));
    simulation.tick();
    assert_ne!(
        inventory(&simulation).inventory_menu.slot(9),
        Some(&stone(8))
    );
}

/// A login or a respawn replaces the server-side player, whose window ids
/// start again. Upstream mineflayer then drops `windowItems` and
/// `lastClosedWindow`, so a new window that reuses an old id does not get
/// the old early content or the old trailing-sync remap.
#[test]
fn login_and_respawn_drop_early_content_and_the_last_closed_container() {
    let _lock = init();
    for reset in ["login", "respawn"] {
        let mut simulation = logged_in();
        open_chest(&mut simulation, 9);
        simulation.trigger(CloseContainerEvent {
            entity: simulation.entity,
            id: 9,
        });
        simulation.receive_packet(content(
            4,
            1,
            vec![ItemStack::Empty; CHEST_LEN],
            ItemStack::Empty,
        ));
        simulation.tick();
        let inv = inventory(&simulation);
        assert!(inv.last_closed.is_some() && inv.early_content.is_some());

        if reset == "login" {
            simulation.receive_packet(default_login_packet());
        } else {
            use mineflayer_registry::DataRegistry;
            simulation.receive_packet(make_basic_respawn_packet(
                mineflayer_registry::data::DimensionKind::new_raw(0),
                mineflayer_registry::identifier::Identifier::new("minecraft:overworld"),
            ));
        }
        simulation.tick();
        let inv = inventory(&simulation);
        assert_eq!(inv.early_content, None, "after {reset}");
        assert_eq!(inv.last_closed, None, "after {reset}");
    }
}
