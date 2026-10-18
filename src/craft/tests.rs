use std::io::Cursor;

use mineflayer_engine::{
    buf::McBuf,
    inventory::ItemStack,
    protocol::{
        common::recipe::{
            Ingredient as WireIngredient, ItemStackSlotDisplay, ShapedCraftingRecipeDisplay,
            ShapelessCraftingRecipeDisplay, SlotDisplayData,
        },
        packets::game::{
            ClientboundGamePacket,
            c_recipe_book_add::{ClientboundRecipeBookAdd, Entry, RecipeDisplayEntry},
            c_recipe_book_remove::ClientboundRecipeBookRemove,
        },
    },
    registry::builtin::RecipeBookCategory,
};

use super::*;

fn item(kind: ItemKind, count: i32, slot: i16) -> Item {
    Item::of(ItemStack::new(kind, count), slot).unwrap()
}

fn stick_recipe() -> Recipe {
    Recipe::new(
        RecipeResult {
            kind: ItemKind::Stick,
            count: 4,
            metadata: None,
        },
        Some(vec![
            vec![Some(Ingredient::item(ItemKind::OakPlanks))],
            vec![Some(Ingredient::item(ItemKind::OakPlanks))],
        ]),
        None,
    )
}

#[test]
fn recipe_new_computes_delta_and_requires_table() {
    let r = stick_recipe();
    // delta: -2 planks +4 sticks
    assert_eq!(r.delta.len(), 2);
    assert_eq!(r.delta[0].count, -2);
    assert_eq!(r.delta[0].any_of[0].kind, ItemKind::OakPlanks);
    assert_eq!(r.delta[1].count, 4);
    assert_eq!(r.delta[1].any_of[0].kind, ItemKind::Stick);
    assert!(!r.requires_table);

    // a 3x2 shaped recipe needs the table on width alone
    let chest = Recipe::new(
        RecipeResult {
            kind: ItemKind::Chest,
            count: 1,
            metadata: None,
        },
        Some(vec![
            vec![
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
                Some(Ingredient::item(ItemKind::OakPlanks)),
            ],
            vec![
                Some(Ingredient::item(ItemKind::OakPlanks)),
                None,
                Some(Ingredient::item(ItemKind::OakPlanks)),
            ],
        ]),
        None,
    );
    assert!(chest.requires_table);

    // 5 loose ingredients don't fit a 2x2 grid either
    let five = Recipe::new(
        RecipeResult {
            kind: ItemKind::Stick,
            count: 1,
            metadata: None,
        },
        None,
        Some(vec![
            Ingredient::item(ItemKind::OakPlanks),
            Ingredient::item(ItemKind::OakPlanks),
            Ingredient::item(ItemKind::OakPlanks),
            Ingredient::item(ItemKind::OakPlanks),
            Ingredient::item(ItemKind::OakPlanks),
        ]),
    );
    assert!(five.requires_table);
}

#[test]
fn unused_recipe_slots_math() {
    // stick shape ([P],[P]) on a 2x2 grid: cells (1,0),(1,1) free
    let r = stick_recipe();
    assert_eq!(unused_recipe_slots(&r, 2, 2), vec![2, 4]);
    // on a 3x3 table the right column and bottom row are free
    assert_eq!(unused_recipe_slots(&r, 3, 3), vec![2, 3, 5, 6, 7, 8, 9]);
    // shapeless: every cell, scan order
    let shapeless = Recipe::new(
        RecipeResult {
            kind: ItemKind::Torch,
            count: 4,
            metadata: None,
        },
        None,
        Some(vec![
            Ingredient::item(ItemKind::Coal),
            Ingredient::item(ItemKind::Stick),
        ]),
    );
    assert_eq!(unused_recipe_slots(&shapeless, 2, 2), vec![1, 2, 3, 4]);
}

#[test]
fn requirements_met_inventory_filter() {
    let r = stick_recipe();
    let counts = HashMap::from([((ItemKind::OakPlanks, None), 3i64)]);
    // 4 sticks need 2 planks → ok at min=1; min=5 needs 2 crafts = 4
    assert!(r.requirements_met(&mut counts.clone(), 1, false));
    assert!(!r.requirements_met(&mut counts.clone(), 5, false));
    // no planks at all
    let mut empty = HashMap::new();
    assert!(!r.requirements_met(&mut empty, 1, false));
    // table-required recipe fails without the table regardless
    let chest = Recipe::new(
        RecipeResult {
            kind: ItemKind::Chest,
            count: 1,
            metadata: None,
        },
        Some(vec![
            vec![Some(Ingredient::item(ItemKind::OakPlanks)); 3],
            vec![None, Some(Ingredient::item(ItemKind::OakPlanks)), None],
            vec![Some(Ingredient::item(ItemKind::OakPlanks)); 3],
        ]),
        None,
    );
    let mut plenty = HashMap::from([((ItemKind::OakPlanks, None), 64i64)]);
    assert!(!chest.requirements_met(&mut plenty.clone(), 1, false));
    assert!(chest.requirements_met(&mut plenty, 1, true));
}

#[test]
fn requirements_met_alternatives() {
    // ingredient accepts any planks kind; only birch is stocked
    let r = Recipe::new(
        RecipeResult {
            kind: ItemKind::CraftingTable,
            count: 1,
            metadata: None,
        },
        None,
        Some(
            (0..4)
                .map(|_| Ingredient {
                    any_of: vec![
                        RecipeItem {
                            kind: ItemKind::OakPlanks,
                            metadata: None,
                        },
                        RecipeItem {
                            kind: ItemKind::BirchPlanks,
                            metadata: None,
                        },
                    ],
                })
                .collect(),
        ),
    );
    let mut counts = HashMap::from([((ItemKind::BirchPlanks, None), 4i64)]);
    assert!(r.requirements_met(&mut counts, 1, false));
    // 4 planks of one kind only reach one craft, not two
    let mut one_short = HashMap::from([((ItemKind::BirchPlanks, None), 3i64)]);
    assert!(!r.requirements_met(&mut one_short, 1, false));
    // same kind can't satisfy two ingredient slots at once
    let mut split = HashMap::from([
        ((ItemKind::BirchPlanks, None), 2i64),
        ((ItemKind::OakPlanks, None), 2i64),
    ]);
    assert!(r.requirements_met(&mut split, 1, false));
}

#[test]
fn ingredient_accepts_metadata_rules() {
    let planks = item(ItemKind::OakPlanks, 5, 9);
    let stick = item(ItemKind::Stick, 4, 10);
    let ing = Ingredient::item(ItemKind::OakPlanks);
    assert!(ing.accepts(&planks));
    assert!(!ing.accepts(&stick));
    // alternatives accept either kind
    let tagged = Ingredient {
        any_of: vec![
            RecipeItem {
                kind: ItemKind::OakPlanks,
                metadata: None,
            },
            RecipeItem {
                kind: ItemKind::BirchPlanks,
                metadata: None,
            },
        ],
    };
    assert!(tagged.accepts(&item(ItemKind::BirchPlanks, 1, 11)));
    // metadata-pinned ingredient rejects undamaged/other items
    let pinned = Ingredient {
        any_of: vec![RecipeItem {
            kind: ItemKind::IronSword,
            metadata: Some(15),
        }],
    };
    assert!(!pinned.accepts(&item(ItemKind::IronSword, 1, 12)));
    let damaged = Item::of(
        ItemStack::new(ItemKind::IronSword, 1)
            .with_component(mineflayer_engine::inventory::components::Damage { amount: 15 }),
        13,
    )
    .unwrap();
    assert!(pinned.accepts(&damaged));
}

fn tag_ingredient(tag: &str) -> WireIngredient {
    WireIngredient {
        allowed: HolderSet::Named {
            key: Identifier::new(tag),
            contents: Vec::new(),
        },
    }
}

fn book_entry(
    id: u32,
    display: RecipeDisplayData,
    requirements: Option<Vec<WireIngredient>>,
) -> Entry {
    Entry {
        contents: RecipeDisplayEntry {
            id,
            display,
            group: 0,
            category: RecipeBookCategory::CraftingMisc,
            crafting_requirements: requirements,
        },
        flags: 0,
    }
}

/// The packet shape vanilla sent on join: 2x2 `Tag(planks)` grid,
/// `ItemStack` result decoded kind/count-swapped by the engine.
fn crafting_table_entry() -> Entry {
    book_entry(
        293,
        RecipeDisplayData::Shaped(ShapedCraftingRecipeDisplay {
            width: 2,
            height: 2,
            ingredients: vec![
                SlotDisplayData::Tag(
                    mineflayer_engine::protocol::common::recipe::TagSlotDisplay {
                        tag: Identifier::new("minecraft:planks"),
                    },
                );
                4
            ],
            result: SlotDisplayData::ItemStack(
                mineflayer_engine::protocol::common::recipe::ItemStackSlotDisplay {
                    stack: ItemStack::new(ItemKind::Stone, 333),
                },
            ),
            crafting_station: SlotDisplayData::Item(
                mineflayer_engine::protocol::common::recipe::ItemSlotDisplay {
                    item: ItemKind::CraftingTable,
                },
            ),
        }),
        Some(vec![tag_ingredient("minecraft:planks"); 4]),
    )
}

/// Pins the engine codec order [`displayed_stack`] compensates for.
/// The mc26 `ItemStackSlotDisplay` wire carries `ItemStackTemplate`
/// item-first: `[kind VarInt][count VarInt][patch]`. The engine 0.16
/// decodes through the container `ItemStack` codec (count-first) —
/// a `crafting_table×1` template therefore decodes as `Stone×333`.
/// When the engine fixes the decode this test fails: delete the un-swap.
#[test]
fn item_stack_slot_display_codec_order() {
    // crafting_table kind id 333 → VarInt 0xCD 0x02; count 1;
    // empty DataComponentPatch (two VarInt zeros)
    let wire: &[u8] = &[0xCD, 0x02, 0x01, 0x00, 0x00];
    let display = ItemStackSlotDisplay::mc_read(&mut Cursor::new(wire)).unwrap();
    let d = display.stack.as_present().expect("non-empty stack");
    assert_eq!(
        (d.count, d.kind),
        (333, ItemKind::Stone),
        "engine's ItemStackSlotDisplay decode changed — remove \
             displayed_stack's un-swap and read kind/count directly"
    );
    // …and the un-swap it exists for still recovers the real pair
    assert_eq!(
        displayed_stack(&display.stack),
        (ItemKind::CraftingTable, 1)
    );
}

#[test]
fn recipe_from_shaped_entry_resolves_tags_and_unswaps_result() {
    let e = crafting_table_entry();
    let entry = BookEntry {
        id: e.contents.id,
        display: e.contents.display.clone(),
        requirements: e.contents.crafting_requirements.clone(),
    };
    let recipe = recipe_from_entry(&entry).expect("shaped converts");
    // The engine decoded the result kind/count swapped — unswap restores it
    assert_eq!(recipe.result.kind, ItemKind::CraftingTable);
    assert_eq!(recipe.result.count, 1);
    // 2x2 shape of #planks cells — resolved through the static tag
    let shape = recipe.in_shape.unwrap();
    assert_eq!(shape.len(), 2);
    assert_eq!(shape[0].len(), 2);
    let cell = shape[0][0].as_ref().unwrap();
    assert!(cell.any_of.len() > 4);
    assert!(cell.any_of.iter().any(|w| w.kind == ItemKind::OakPlanks));
    // 4 plank cells, 1 table → delta -4 +1; fits 2x2 → no table
    assert_eq!(recipe.delta.len(), 2);
    assert_eq!(recipe.delta[0].count, -4);
    assert!(!recipe.requires_table);
}

#[test]
fn recipe_from_shapeless_entry_uses_requirements() {
    let e = book_entry(
        42,
        RecipeDisplayData::Shapeless(ShapelessCraftingRecipeDisplay {
            ingredients: vec![SlotDisplayData::Empty; 2],
            result: SlotDisplayData::Item(
                mineflayer_engine::protocol::common::recipe::ItemSlotDisplay {
                    item: ItemKind::Stick,
                },
            ),
            crafting_station: SlotDisplayData::Empty,
        }),
        Some(vec![
            tag_ingredient("minecraft:planks"),
            WireIngredient {
                allowed: HolderSet::Direct {
                    contents: vec![ItemKind::Coal],
                },
            },
        ]),
    );
    let entry = BookEntry {
        id: e.contents.id,
        display: e.contents.display.clone(),
        requirements: e.contents.crafting_requirements.clone(),
    };
    let recipe = recipe_from_entry(&entry).expect("shapeless converts");
    assert_eq!(recipe.result.kind, ItemKind::Stick);
    assert_eq!(recipe.result.count, 1);
    let ings = recipe.ingredients.unwrap();
    assert_eq!(ings.len(), 2);
    assert!(ings[0].any_of.len() > 4); // #planks
    assert_eq!(ings[1].any_of.len(), 1); // direct coal
    assert!(recipe.in_shape.is_none());
}

#[test]
fn book_add_replace_remove() {
    let book = Arc::new(Mutex::new(RecipeBook::default()));
    let add = Event::Packet(Arc::new(ClientboundGamePacket::RecipeBookAdd(
        ClientboundRecipeBookAdd {
            entries: vec![crafting_table_entry()],
            replace: true,
        },
    )));
    observe_event(&book, &add);
    assert_eq!(book.lock().entries.len(), 1);
    // same id again without replace dedupes
    let add_again = Event::Packet(Arc::new(ClientboundGamePacket::RecipeBookAdd(
        ClientboundRecipeBookAdd {
            entries: vec![crafting_table_entry()],
            replace: false,
        },
    )));
    observe_event(&book, &add_again);
    assert_eq!(book.lock().entries.len(), 1);
    // remove drops it
    let remove = Event::Packet(Arc::new(ClientboundGamePacket::RecipeBookRemove(
        ClientboundRecipeBookRemove { recipes: vec![293] },
    )));
    observe_event(&book, &remove);
    assert!(book.lock().entries.is_empty());
}
