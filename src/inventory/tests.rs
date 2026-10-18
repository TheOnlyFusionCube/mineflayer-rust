
use super::*;
use mineflayer_engine::inventory::{components::Damage, operations::CloneClick};

/// `unequip(Hand)` on a despawned client must return `NotConnected`.
/// The baseline read `slots().unwrap_or_default()` and indexed the
/// empty vector (`slots[QUICK_BAR_START]`), which panicked.
/// The integer `click_window` and the typed `click_window_operation`
/// share one crafting-wait rule, keyed on the operation's slot.
#[test]
fn crafting_wait_rule_follows_the_operation_slot() {
    assert!(waits_for_craft_result(0, None, Some(0)));
    assert!(waits_for_craft_result(0, None, Some(4)));
    assert!(!waits_for_craft_result(0, None, Some(5)));
    assert!(waits_for_craft_result(3, Some(MenuKind::Crafting), Some(9)));
    assert!(!waits_for_craft_result(
        3,
        Some(MenuKind::Crafting),
        Some(10)
    ));
    assert!(!waits_for_craft_result(
        3,
        Some(MenuKind::Generic9x3),
        Some(0)
    ));
    // outside clicks (-999) never wait
    assert!(!waits_for_craft_result(0, None, None));
    let pickup = click_operation(0, 0, 0).unwrap();
    assert_eq!(pickup.slot_num(), Some(0));
    assert_eq!(
        ClickOperation::Pickup(PickupClick::LeftOutside).slot_num(),
        None
    );
}

#[tokio::test]
async fn unequip_hand_without_inventory_is_not_connected() {
    let bot = crate::test_util::bot(crate::test_util::despawned_client());
    let err = bot.unequip(EquipDest::Hand).await.unwrap_err();
    assert!(matches!(err, Error::NotConnected), "got {err:?}");
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
    // mode 3: middle/clone click — the true click_type 3
    assert!(matches!(
        click_operation(10, 0, 3).unwrap(),
        Clone(CloneClick { slot: 10 })
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
    // QuickCraft/PickupAll stay unreachable from the public mapping
    // — documenting the coverage boundary, no test value needed.
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

fn stack(kind: ItemKind, count: i32) -> mineflayer_engine::inventory::ItemStack {
    mineflayer_engine::inventory::ItemStack::new(kind, count)
}

#[test]
fn item_matching_and_finders() {
    use crate::windows::{find_item_in, first_empty_in};
    let slots = vec![
        stack(ItemKind::Stone, 32),
        mineflayer_engine::inventory::ItemStack::Empty,
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
    let damaged = find_item_in(&slots, 0..5, ItemKind::IronSword, Some(15), false, None).unwrap();
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
    assert!(Item::of(mineflayer_engine::inventory::ItemStack::Empty, 0).is_none());

    // same_item ignores count but not kind or components
    let a = Item::of(stack(ItemKind::Stone, 1), 0).unwrap();
    let b = Item::of(stack(ItemKind::Stone, 64), 1).unwrap();
    assert!(a.same_item(&b));
    let c = Item::of(stack(ItemKind::Dirt, 1), 2).unwrap();
    assert!(!a.same_item(&c));
}
