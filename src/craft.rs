//! Crafting and recipes — the `craft.js` + `prismarine-recipe` port.
//!
//! # Where recipes come from
//!
//! Upstream reads a static minecraft-data table; this port reads the
//! server's own recipe book: mc26 sends `c_recipe_book_add` with full
//! `RecipeDisplayEntry`s (shape, per-slot alternatives, result) as
//! recipes unlock, and `c_recipe_book_remove` retracts them. The bridge
//! records both into `RecipeBook`; `recipes_for`/`recipes_all` convert
//! the crafting displays into [`Recipe`]s. The book only contains what
//! the server has unlocked — a fresh vanilla join knows one recipe
//! (crafting table) and learns more as the inventory changes.
//!
//! `craft` itself is book-free, exactly like upstream's `craft.js`: it
//! clicks the 2x2 player grid or a 3x3 table window and takes the
//! result — no `s_place_recipe`. [`Recipe`] values can always be
//! constructed by hand (`Recipe::new`), so crafting works even where the
//! book is empty.
//!
//! # Recipe model vs upstream
//!
//! Upstream `RecipeItem` is a single `{id, metadata, count}`; real
//! vanilla ingredients are alternative *sets* (`#minecraft:planks` and
//! friends), so ours is [`Ingredient`]`{ any_of: Vec<RecipeItem> }`.
//! Tag alternatives resolve through the engine's static vanilla item tags
//! (`c_update_tags` arrives in the config phase, which `Event::Packet`
//! never sees — the statics are the same data).

use std::{collections::HashMap, str::FromStr, sync::Arc};

use parking_lot::Mutex;

use crate::{
    Bot, Error, Result,
    inventory::click_window_on,
    windows::{Item, Window},
    world::Block,
};
use mineflayer_engine::{
    Event,
    protocol::{
        common::recipe::{Ingredient as WireIngredient, RecipeDisplayData, SlotDisplayData},
        packets::game::ClientboundGamePacket,
    },
    registry::{
        HolderSet, Registry,
        builtin::{ItemKind, MenuKind},
        identifier::Identifier,
        tags::{RegistryTag, items},
    },
};

/// One acceptable item in a recipe slot — upstream `RecipeItem`
/// (`{id, metadata}`; `count` is always 1 in crafting grids, so it
/// isn't carried). `metadata` is the `damage` component like
/// [`Item::metadata`]; `None` matches any damage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecipeItem {
    /// The item kind (upstream `id` as an enum).
    pub kind: ItemKind,
    /// Required `damage` component, `None` = any.
    pub metadata: Option<i32>,
}

/// One ingredient slot — the set of items that may satisfy it. Usually
/// one entry; tag/composite ingredients list every member.
#[derive(Clone, Debug, PartialEq)]
pub struct Ingredient {
    /// Acceptable items (upstream resolves tag recipes into separate
    /// recipe entries instead of having this set).
    pub any_of: Vec<RecipeItem>,
}

impl Ingredient {
    /// One-kind ingredient — the common case.
    pub fn item(kind: ItemKind) -> Self {
        Ingredient {
            any_of: vec![RecipeItem {
                kind,
                metadata: None,
            }],
        }
    }

    /// Whether `item` satisfies this ingredient — upstream's
    /// `findInventoryItem(type, metadata)` predicate (kind equal,
    /// `metadata` null-or-equal).
    pub fn accepts(&self, item: &Item) -> bool {
        self.any_of.iter().any(|w| {
            w.kind == item.kind() && (w.metadata.is_none() || w.metadata == item.metadata())
        })
    }
}

/// What one craft produces — upstream `recipe.result`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecipeResult {
    /// The produced item kind.
    pub kind: ItemKind,
    /// Items produced per craft.
    pub count: i32,
    /// `damage` component of the result, if the recipe pins one.
    pub metadata: Option<i32>,
}

/// One net inventory change per craft — upstream `recipe.delta`
/// entries. Negative `count` consumes, positive produces.
#[derive(Clone, Debug, PartialEq)]
pub struct DeltaEntry {
    /// Kinds this entry can draw from/produce (alternatives for tag
    /// ingredients; single-kind for concrete ones).
    pub any_of: Vec<RecipeItem>,
    /// Net change per craft.
    pub count: i32,
}

/// A crafting recipe — the `prismarine-recipe` `Recipe` analog.
#[derive(Clone, Debug, PartialEq)]
pub struct Recipe {
    /// Produced item.
    pub result: RecipeResult,
    /// Shaped layout: rows of optional ingredient cells (upstream
    /// `inShape`, `null`/`id:-1` cells → `None`). `None` for shapeless.
    pub in_shape: Option<Vec<Vec<Option<Ingredient>>>>,
    /// Grid contents left after the craft (upstream `outShape`) —
    /// container-item remainders like buckets. The server's displays
    /// don't carry this (`WithRemainder` wraps the input instead) and
    /// `craft` clears whatever the grid actually holds, so this is
    /// only populated by hand-built recipes. Note [`Recipe::new`]
    /// computes `delta` once — setting `out_shape` afterwards does
    /// NOT fold upstream's `+1` remainder entries into `delta`, and
    /// upstream's `normalizeMetadata` wildcard rule (metadata ≥32767
    /// → null) is not ported.
    pub out_shape: Option<Vec<Vec<Option<Ingredient>>>>,
    /// Shapeless ingredient list (upstream `ingredients`). `None` for
    /// shaped recipes.
    pub ingredients: Option<Vec<Ingredient>>,
    /// Net inventory change per craft (upstream `delta`).
    pub delta: Vec<DeltaEntry>,
    /// Whether the recipe needs a 3x3 table (upstream
    /// `requiresTable`: shape taller/wider than 2 or more than 4
    /// occupied cells/ingredients).
    pub requires_table: bool,
}

impl Recipe {
    /// Build a recipe from result + shape-or-ingredients, computing
    /// `delta` and `requires_table` like `prismarine-recipe`'s
    /// constructor (`computeDelta`/`computeRequiresTable`).
    pub fn new(
        result: RecipeResult,
        in_shape: Option<Vec<Vec<Option<Ingredient>>>>,
        ingredients: Option<Vec<Ingredient>>,
    ) -> Self {
        let mut delta = Vec::new();
        let mut space_left = 4i32;
        let mut requires_table = false;
        if let Some(shape) = &in_shape {
            if shape.len() > 2 || shape.iter().any(|r| r.len() > 2) {
                requires_table = true;
            }
            for row in shape {
                for cell in row.iter().flatten() {
                    space_left -= 1;
                    add_delta(&mut delta, cell.any_of.clone(), -1);
                }
            }
        }
        if let Some(ings) = &ingredients {
            space_left -= ings.len() as i32;
            for ing in ings {
                add_delta(&mut delta, ing.any_of.clone(), -1);
            }
        }
        requires_table |= space_left < 0;
        add_delta(
            &mut delta,
            vec![RecipeItem {
                kind: result.kind,
                metadata: result.metadata,
            }],
            result.count,
        );
        Recipe {
            result,
            in_shape,
            out_shape: None,
            ingredients,
            delta,
            requires_table,
        }
    }

    /// Every ingredient slot one craft consumes — occupied shape cells
    /// plus loose ingredients (upstream's delta-negative side).
    fn demands(&self) -> impl Iterator<Item = &Ingredient> {
        self.in_shape
            .iter()
            .flat_map(|shape| shape.iter().flat_map(|row| row.iter().flatten()))
            .chain(self.ingredients.iter().flatten())
    }

    /// Upstream `requirementsMetForRecipe`: table gate plus a simulated
    /// consume over [`Self::demands`] — the delta check generalized to
    /// alternative sets (`counts` is consumed, so pass a scratch map).
    fn requirements_met(
        &self,
        counts: &mut HashMap<(ItemKind, Option<i32>), i64>,
        min_result_count: i32,
        has_table: bool,
    ) -> bool {
        if self.requires_table && !has_table {
            return false;
        }
        let craft_count =
            (min_result_count + self.result.count.max(1) - 1) / self.result.count.max(1);
        for _ in 0..craft_count {
            for ing in self.demands() {
                if !consume_one(counts, ing) {
                    return false;
                }
            }
        }
        true
    }
}

fn add_delta(delta: &mut Vec<DeltaEntry>, any_of: Vec<RecipeItem>, count: i32) {
    if let Some(d) = delta.iter_mut().find(|d| d.any_of == any_of) {
        d.count += count;
    } else {
        delta.push(DeltaEntry { any_of, count });
    }
}

/// Take one unit of any kind the ingredient accepts from `counts`.
/// Greedy over `any_of` order — two demand slots sharing an
/// alternative under scarcity can falsely fail (bipartite matching
/// would be exact; upstream never needed it with single-kind deltas
/// and it's rare for vanilla tag ingredients).
fn consume_one(counts: &mut HashMap<(ItemKind, Option<i32>), i64>, ing: &Ingredient) -> bool {
    for want in &ing.any_of {
        let key = counts
            .keys()
            .find(|(kind, meta)| {
                *kind == want.kind && (want.metadata.is_none() || *meta == want.metadata)
            })
            .copied();
        let Some(key) = key else { continue };
        let stock = counts.get_mut(&key).unwrap();
        if *stock > 0 {
            *stock -= 1;
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Recipe book — the bridge-kept list of unlocked recipes.
// ---------------------------------------------------------------------------

/// One recipe-book entry as decoded from `c_recipe_book_add`.
pub(crate) struct BookEntry {
    /// The server's recipe display id.
    pub id: u32,
    /// The shape/result display payload.
    pub display: RecipeDisplayData,
    /// `crafting_requirements` — the canonical ingredient list for
    /// shapeless entries (shaped entries carry it too but the grid
    /// display is authoritative for position).
    pub requirements: Option<Vec<WireIngredient>>,
}

/// The recipes this server has told us we know — upstream's static
/// `Recipe.find` source, kept in lockstep with `c_recipe_book_add`/
/// `c_recipe_book_remove` by [`observe_event`]. Shared `Bot` ↔ bridge.
#[derive(Default)]
pub(crate) struct RecipeBook {
    /// Entries in arrival order (deduplicated by id).
    pub entries: Vec<BookEntry>,
}

/// Record recipe-book packets from one engine event — no `BotEvent`s
/// (craft emits none upstream either). Called from the bridge loop.
pub(crate) fn observe_event(book: &Arc<Mutex<RecipeBook>>, event: &Event) {
    let Event::Packet(packet) = event else {
        return;
    };
    match &**packet {
        ClientboundGamePacket::RecipeBookAdd(p) => {
            let mut book = book.lock();
            if p.replace {
                book.entries.clear();
            }
            for e in &p.entries {
                book.entries.retain(|x| x.id != e.contents.id);
                book.entries.push(BookEntry {
                    id: e.contents.id,
                    display: e.contents.display.clone(),
                    requirements: e.contents.crafting_requirements.clone(),
                });
            }
        }
        ClientboundGamePacket::RecipeBookRemove(p) => {
            book.lock().entries.retain(|e| !p.recipes.contains(&e.id));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// RecipeDisplay → Recipe conversion.
// ---------------------------------------------------------------------------

fn recipe_from_entry(e: &BookEntry) -> Option<Recipe> {
    match &e.display {
        RecipeDisplayData::Shaped(d) => {
            let (w, h) = (d.width as usize, d.height as usize);
            if w == 0 || h == 0 {
                return None;
            }
            let mut shape = Vec::with_capacity(h);
            for y in 0..h {
                let mut row = Vec::with_capacity(w);
                for x in 0..w {
                    row.push(
                        d.ingredients
                            .get(y * w + x)
                            .and_then(slot_display_ingredient),
                    );
                }
                shape.push(row);
            }
            Some(Recipe::new(
                slot_display_result(&d.result)?,
                Some(shape),
                None,
            ))
        }
        RecipeDisplayData::Shapeless(d) => {
            // `crafting_requirements` is the canonical ingredient list;
            // fall back to the display's own slots when absent.
            let ings: Vec<Ingredient> = match &e.requirements {
                Some(reqs) if !reqs.is_empty() => reqs
                    .iter()
                    .map(|i| Ingredient {
                        any_of: holder_set_items(&i.allowed),
                    })
                    .collect(),
                _ => d
                    .ingredients
                    .iter()
                    .filter_map(slot_display_ingredient)
                    .collect(),
            };
            Some(Recipe::new(
                slot_display_result(&d.result)?,
                None,
                Some(ings),
            ))
        }
        // furnace/stonecutter/smithing aren't `craft` recipes
        _ => None,
    }
}

/// The acceptable items of a non-empty [`SlotDisplayData`].
fn slot_display_items(d: &SlotDisplayData) -> Vec<RecipeItem> {
    match d {
        SlotDisplayData::Empty | SlotDisplayData::AnyFuel => Vec::new(),
        SlotDisplayData::Item(i) => vec![RecipeItem {
            kind: i.item,
            metadata: None,
        }],
        SlotDisplayData::ItemStack(s) => {
            let (kind, _) = displayed_stack(&s.stack);
            vec![RecipeItem {
                kind,
                metadata: stack_metadata(&s.stack),
            }]
        }
        SlotDisplayData::Tag(t) => tag_items(&t.tag),
        SlotDisplayData::Composite(c) => c.contents.iter().flat_map(slot_display_items).collect(),
        SlotDisplayData::WithRemainder(w) => slot_display_items(&w.input),
        SlotDisplayData::WithAnyPotion(w) => slot_display_items(&w.contents),
        SlotDisplayData::OnlyWithComponent(o) => slot_display_items(&o.contents),
        SlotDisplayData::Dyed(d) => slot_display_items(&d.target),
        SlotDisplayData::SmithingTrim(s) => slot_display_items(&s.material),
    }
}

/// A slot display as a grid ingredient (`None` for empty cells).
fn slot_display_ingredient(d: &SlotDisplayData) -> Option<Ingredient> {
    let any_of = slot_display_items(d);
    (!any_of.is_empty()).then_some(Ingredient { any_of })
}

/// The recipe result — `Item`/`ItemStack` displays only.
fn slot_display_result(d: &SlotDisplayData) -> Option<RecipeResult> {
    match d {
        SlotDisplayData::Item(i) => Some(RecipeResult {
            kind: i.item,
            count: 1,
            metadata: None,
        }),
        SlotDisplayData::ItemStack(s) => {
            let (kind, count) = displayed_stack(&s.stack);
            Some(RecipeResult {
                kind,
                count,
                metadata: stack_metadata(&s.stack),
            })
        }
        _ => None,
    }
}

/// The engine 0.16's `ItemStack` codec reads `count` then `kind`
/// (`mineflayer-inventory/src/slot.rs:226-239`, `impl McBuf for ItemStack`),
/// but mc26's `ItemStack` slot displays serialize `ItemStackTemplate`
/// `(item, count, components)` item-first — the decoded pair arrives
/// swapped (`Stone×333` for `crafting_table×1`). Recover by treating
/// the decoded count as the real kind id.
///
/// Removal condition: this is a confirmed upstream decode bug. When
/// The engine fixes `ItemStackSlotDisplay` (a dedicated template codec
/// reading item-first), the pinning test
/// `item_stack_slot_display_codec_order` fails — delete this un-swap
/// and read `d.kind`/`d.count` directly. The `from_u32` fallback can
/// never fire under a fixed codec (counts 1..=99 always map to a
/// valid kind), so without the test this corrupts silently.
fn displayed_stack(stack: &mineflayer_engine::inventory::ItemStack) -> (ItemKind, i32) {
    let Some(d) = stack.as_present() else {
        return (ItemKind::Air, 0);
    };
    match ItemKind::from_u32(d.count.max(0) as u32) {
        Some(kind) => (kind, d.kind.to_u32() as i32),
        None => (d.kind, d.count),
    }
}

/// The `damage` component as upstream `metadata` (the patch decodes
/// fine regardless of the kind/count swap).
fn stack_metadata(stack: &mineflayer_engine::inventory::ItemStack) -> Option<i32> {
    stack
        .get_component::<mineflayer_engine::inventory::components::Damage>()
        .map(|d| d.amount)
}

fn holder_set_items(h: &HolderSet<ItemKind, Identifier>) -> Vec<RecipeItem> {
    match h {
        HolderSet::Direct { contents } => contents
            .iter()
            .map(|kind| RecipeItem {
                kind: *kind,
                metadata: None,
            })
            .collect(),
        // `Named.contents` is empty on the wire — resolve through the
        // static vanilla tags (same data `c_update_tags` carries in
        // the config phase).
        HolderSet::Named { key, contents } if contents.is_empty() => tag_items(key),
        HolderSet::Named { key: _, contents } => contents
            .iter()
            .filter_map(|id| ItemKind::from_str(&id.to_string()).ok())
            .map(|kind| RecipeItem {
                kind,
                metadata: None,
            })
            .collect(),
    }
}

/// `minecraft:<path>` → the static vanilla item tag's members.
fn tag_items(id: &Identifier) -> Vec<RecipeItem> {
    item_tag(id)
        .map(|tag| {
            tag.iter()
                .map(|kind| RecipeItem {
                    kind: *kind,
                    metadata: None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `minecraft` item tags the engine ships as generated statics —
/// vanilla `c_update_tags` carries the same contents but in the
/// config phase, which never reaches `Event::Packet`.
fn item_tag(id: &Identifier) -> Option<&'static RegistryTag<ItemKind>> {
    if id.namespace() != "minecraft" {
        return None;
    }
    Some(match id.path() {
        "acacia_logs" => &items::ACACIA_LOGS,
        "anvil" => &items::ANVIL,
        "armadillo_food" => &items::ARMADILLO_FOOD,
        "arrows" => &items::ARROWS,
        "axes" => &items::AXES,
        "axolotl_food" => &items::AXOLOTL_FOOD,
        "bamboo_blocks" => &items::BAMBOO_BLOCKS,
        "banners" => &items::BANNERS,
        "bars" => &items::BARS,
        "beacon_payment_items" => &items::BEACON_PAYMENT_ITEMS,
        "beds" => &items::BEDS,
        "bee_food" => &items::BEE_FOOD,
        "birch_logs" => &items::BIRCH_LOGS,
        "boats" => &items::BOATS,
        "book_cloning_target" => &items::BOOK_CLONING_TARGET,
        "bookshelf_books" => &items::BOOKSHELF_BOOKS,
        "breaks_decorated_pots" => &items::BREAKS_DECORATED_POTS,
        "brewing_fuel" => &items::BREWING_FUEL,
        "bundles" => &items::BUNDLES,
        "buttons" => &items::BUTTONS,
        "camel_food" => &items::CAMEL_FOOD,
        "camel_husk_food" => &items::CAMEL_HUSK_FOOD,
        "candles" => &items::CANDLES,
        "cat_collar_dyes" => &items::CAT_COLLAR_DYES,
        "cat_food" => &items::CAT_FOOD,
        "cauldron_can_remove_dye" => &items::CAULDRON_CAN_REMOVE_DYE,
        "chains" => &items::CHAINS,
        "cherry_logs" => &items::CHERRY_LOGS,
        "chest_armor" => &items::CHEST_ARMOR,
        "chest_boats" => &items::CHEST_BOATS,
        "chicken_food" => &items::CHICKEN_FOOD,
        "cluster_max_harvestables" => &items::CLUSTER_MAX_HARVESTABLES,
        "coal_ores" => &items::COAL_ORES,
        "coals" => &items::COALS,
        "compasses" => &items::COMPASSES,
        "completes_find_tree_tutorial" => &items::COMPLETES_FIND_TREE_TUTORIAL,
        "copper" => &items::COPPER,
        "copper_chests" => &items::COPPER_CHESTS,
        "copper_golem_statues" => &items::COPPER_GOLEM_STATUES,
        "copper_ores" => &items::COPPER_ORES,
        "copper_tool_materials" => &items::COPPER_TOOL_MATERIALS,
        "cow_food" => &items::COW_FOOD,
        "creeper_drop_music_discs" => &items::CREEPER_DROP_MUSIC_DISCS,
        "creeper_igniters" => &items::CREEPER_IGNITERS,
        "crimson_stems" => &items::CRIMSON_STEMS,
        "dampens_vibrations" => &items::DAMPENS_VIBRATIONS,
        "dark_oak_logs" => &items::DARK_OAK_LOGS,
        "decorated_pot_ingredients" => &items::DECORATED_POT_INGREDIENTS,
        "decorated_pot_sherds" => &items::DECORATED_POT_SHERDS,
        "diamond_ores" => &items::DIAMOND_ORES,
        "diamond_tool_materials" => &items::DIAMOND_TOOL_MATERIALS,
        "dirt" => &items::DIRT,
        "doors" => &items::DOORS,
        "drowned_preferred_weapons" => &items::DROWNED_PREFERRED_WEAPONS,
        "duplicates_allays" => &items::DUPLICATES_ALLAYS,
        "dyes" => &items::DYES,
        "eggs" => &items::EGGS,
        "emerald_ores" => &items::EMERALD_ORES,
        "enchantable_armor" => &items::ENCHANTABLE_ARMOR,
        "enchantable_bow" => &items::ENCHANTABLE_BOW,
        "enchantable_chest_armor" => &items::ENCHANTABLE_CHEST_ARMOR,
        "enchantable_crossbow" => &items::ENCHANTABLE_CROSSBOW,
        "enchantable_durability" => &items::ENCHANTABLE_DURABILITY,
        "enchantable_equippable" => &items::ENCHANTABLE_EQUIPPABLE,
        "enchantable_fire_aspect" => &items::ENCHANTABLE_FIRE_ASPECT,
        "enchantable_fishing" => &items::ENCHANTABLE_FISHING,
        "enchantable_foot_armor" => &items::ENCHANTABLE_FOOT_ARMOR,
        "enchantable_head_armor" => &items::ENCHANTABLE_HEAD_ARMOR,
        "enchantable_leg_armor" => &items::ENCHANTABLE_LEG_ARMOR,
        "enchantable_lunge" => &items::ENCHANTABLE_LUNGE,
        "enchantable_mace" => &items::ENCHANTABLE_MACE,
        "enchantable_melee_weapon" => &items::ENCHANTABLE_MELEE_WEAPON,
        "enchantable_mining" => &items::ENCHANTABLE_MINING,
        "enchantable_mining_loot" => &items::ENCHANTABLE_MINING_LOOT,
        "enchantable_sharp_weapon" => &items::ENCHANTABLE_SHARP_WEAPON,
        "enchantable_sweeping" => &items::ENCHANTABLE_SWEEPING,
        "enchantable_trident" => &items::ENCHANTABLE_TRIDENT,
        "enchantable_vanishing" => &items::ENCHANTABLE_VANISHING,
        "enchantable_weapon" => &items::ENCHANTABLE_WEAPON,
        "fence_gates" => &items::FENCE_GATES,
        "fences" => &items::FENCES,
        "fishes" => &items::FISHES,
        "flowers" => &items::FLOWERS,
        "foot_armor" => &items::FOOT_ARMOR,
        "fox_food" => &items::FOX_FOOD,
        "freeze_immune_wearables" => &items::FREEZE_IMMUNE_WEARABLES,
        "frog_food" => &items::FROG_FOOD,
        "furnace_minecart_fuel" => &items::FURNACE_MINECART_FUEL,
        "gaze_disguise_equipment" => &items::GAZE_DISGUISE_EQUIPMENT,
        "goat_food" => &items::GOAT_FOOD,
        "gold_ores" => &items::GOLD_ORES,
        "gold_tool_materials" => &items::GOLD_TOOL_MATERIALS,
        "grass_blocks" => &items::GRASS_BLOCKS,
        "hanging_signs" => &items::HANGING_SIGNS,
        "happy_ghast_food" => &items::HAPPY_GHAST_FOOD,
        "happy_ghast_tempt_items" => &items::HAPPY_GHAST_TEMPT_ITEMS,
        "harnesses" => &items::HARNESSES,
        "head_armor" => &items::HEAD_ARMOR,
        "hoes" => &items::HOES,
        "hoglin_food" => &items::HOGLIN_FOOD,
        "horse_food" => &items::HORSE_FOOD,
        "horse_tempt_items" => &items::HORSE_TEMPT_ITEMS,
        "ignored_by_piglin_babies" => &items::IGNORED_BY_PIGLIN_BABIES,
        "iron_ores" => &items::IRON_ORES,
        "iron_tool_materials" => &items::IRON_TOOL_MATERIALS,
        "jungle_logs" => &items::JUNGLE_LOGS,
        "lanterns" => &items::LANTERNS,
        "lapis_ores" => &items::LAPIS_ORES,
        "leaves" => &items::LEAVES,
        "lectern_books" => &items::LECTERN_BOOKS,
        "leg_armor" => &items::LEG_ARMOR,
        "lightning_rods" => &items::LIGHTNING_RODS,
        "llama_food" => &items::LLAMA_FOOD,
        "llama_tempt_items" => &items::LLAMA_TEMPT_ITEMS,
        "logs" => &items::LOGS,
        "logs_that_burn" => &items::LOGS_THAT_BURN,
        "loom_dyes" => &items::LOOM_DYES,
        "loom_patterns" => &items::LOOM_PATTERNS,
        "mangrove_logs" => &items::MANGROVE_LOGS,
        "map_invisibility_equipment" => &items::MAP_INVISIBILITY_EQUIPMENT,
        "meat" => &items::MEAT,
        "metal_nuggets" => &items::METAL_NUGGETS,
        "moss_blocks" => &items::MOSS_BLOCKS,
        "mud" => &items::MUD,
        "nautilus_bucket_food" => &items::NAUTILUS_BUCKET_FOOD,
        "nautilus_food" => &items::NAUTILUS_FOOD,
        "nautilus_taming_items" => &items::NAUTILUS_TAMING_ITEMS,
        "netherite_tool_materials" => &items::NETHERITE_TOOL_MATERIALS,
        "non_flammable_wood" => &items::NON_FLAMMABLE_WOOD,
        "noteblock_top_instruments" => &items::NOTEBLOCK_TOP_INSTRUMENTS,
        "oak_logs" => &items::OAK_LOGS,
        "ocelot_food" => &items::OCELOT_FOOD,
        "pale_oak_logs" => &items::PALE_OAK_LOGS,
        "panda_eats_from_ground" => &items::PANDA_EATS_FROM_GROUND,
        "panda_food" => &items::PANDA_FOOD,
        "parrot_food" => &items::PARROT_FOOD,
        "parrot_poisonous_food" => &items::PARROT_POISONOUS_FOOD,
        "pickaxes" => &items::PICKAXES,
        "pig_food" => &items::PIG_FOOD,
        "piglin_food" => &items::PIGLIN_FOOD,
        "piglin_loved" => &items::PIGLIN_LOVED,
        "piglin_preferred_weapons" => &items::PIGLIN_PREFERRED_WEAPONS,
        "piglin_repellents" => &items::PIGLIN_REPELLENTS,
        "piglin_safe_armor" => &items::PIGLIN_SAFE_ARMOR,
        "pillager_preferred_weapons" => &items::PILLAGER_PREFERRED_WEAPONS,
        "planks" => &items::PLANKS,
        "rabbit_food" => &items::RABBIT_FOOD,
        "rails" => &items::RAILS,
        "redstone_ores" => &items::REDSTONE_ORES,
        "repairs_chain_armor" => &items::REPAIRS_CHAIN_ARMOR,
        "repairs_copper_armor" => &items::REPAIRS_COPPER_ARMOR,
        "repairs_diamond_armor" => &items::REPAIRS_DIAMOND_ARMOR,
        "repairs_gold_armor" => &items::REPAIRS_GOLD_ARMOR,
        "repairs_iron_armor" => &items::REPAIRS_IRON_ARMOR,
        "repairs_leather_armor" => &items::REPAIRS_LEATHER_ARMOR,
        "repairs_netherite_armor" => &items::REPAIRS_NETHERITE_ARMOR,
        "repairs_turtle_helmet" => &items::REPAIRS_TURTLE_HELMET,
        "repairs_wolf_armor" => &items::REPAIRS_WOLF_ARMOR,
        "sand" => &items::SAND,
        "saplings" => &items::SAPLINGS,
        "shearable_from_copper_golem" => &items::SHEARABLE_FROM_COPPER_GOLEM,
        "sheep_food" => &items::SHEEP_FOOD,
        "shovels" => &items::SHOVELS,
        "shulker_boxes" => &items::SHULKER_BOXES,
        "signs" => &items::SIGNS,
        "skeleton_preferred_weapons" => &items::SKELETON_PREFERRED_WEAPONS,
        "skulls" => &items::SKULLS,
        "slabs" => &items::SLABS,
        "small_flowers" => &items::SMALL_FLOWERS,
        "smelts_to_glass" => &items::SMELTS_TO_GLASS,
        "sniffer_food" => &items::SNIFFER_FOOD,
        "soul_fire_base_blocks" => &items::SOUL_FIRE_BASE_BLOCKS,
        "spears" => &items::SPEARS,
        "spruce_logs" => &items::SPRUCE_LOGS,
        "stairs" => &items::STAIRS,
        "stone_bricks" => &items::STONE_BRICKS,
        "stone_buttons" => &items::STONE_BUTTONS,
        "stone_crafting_materials" => &items::STONE_CRAFTING_MATERIALS,
        "stone_tool_materials" => &items::STONE_TOOL_MATERIALS,
        "strider_food" => &items::STRIDER_FOOD,
        "strider_tempt_items" => &items::STRIDER_TEMPT_ITEMS,
        "swords" => &items::SWORDS,
        "terracotta" => &items::TERRACOTTA,
        "trapdoors" => &items::TRAPDOORS,
        "trim_materials" => &items::TRIM_MATERIALS,
        "trimmable_armor" => &items::TRIMMABLE_ARMOR,
        "turtle_food" => &items::TURTLE_FOOD,
        "villager_picks_up" => &items::VILLAGER_PICKS_UP,
        "villager_plantable_seeds" => &items::VILLAGER_PLANTABLE_SEEDS,
        "walls" => &items::WALLS,
        "warped_stems" => &items::WARPED_STEMS,
        "wart_blocks" => &items::WART_BLOCKS,
        "wither_skeleton_disliked_weapons" => &items::WITHER_SKELETON_DISLIKED_WEAPONS,
        "wolf_collar_dyes" => &items::WOLF_COLLAR_DYES,
        "wolf_food" => &items::WOLF_FOOD,
        "wooden_buttons" => &items::WOODEN_BUTTONS,
        "wooden_doors" => &items::WOODEN_DOORS,
        "wooden_fences" => &items::WOODEN_FENCES,
        "wooden_pressure_plates" => &items::WOODEN_PRESSURE_PLATES,
        "wooden_shelves" => &items::WOODEN_SHELVES,
        "wooden_slabs" => &items::WOODEN_SLABS,
        "wooden_stairs" => &items::WOODEN_STAIRS,
        "wooden_tool_materials" => &items::WOODEN_TOOL_MATERIALS,
        "wooden_trapdoors" => &items::WOODEN_TRAPDOORS,
        "wool" => &items::WOOL,
        "wool_carpets" => &items::WOOL_CARPETS,
        "zombie_horse_food" => &items::ZOMBIE_HORSE_FOOD,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Bot surface — recipesFor/recipesAll/craft.
// ---------------------------------------------------------------------------

/// `(kind, damage)` → item count over the whole inventory window —
/// upstream `requirementsMetForRecipe` uses `bot.inventory.count`,
/// which counts every slot (crafting grid 1-4, armor 5-8, main 9-44,
/// offhand 45, and a mid-craft result in slot 0), not just the player
/// region.
fn inventory_counts(window: &Window) -> HashMap<(ItemKind, Option<i32>), i64> {
    let mut counts = HashMap::new();
    for item in window.items() {
        *counts.entry((item.kind(), item.metadata())).or_insert(0) += i64::from(item.count());
    }
    counts
}

/// The upstream result-metadata quirk: `Recipe.find` matches when the
/// filter is null OR the recipe's result has no `meta` field OR equal.
fn result_matches(r: &Recipe, kind: ItemKind, metadata: Option<i32>) -> bool {
    r.result.kind == kind
        && (metadata.is_none() || r.result.metadata.is_none() || r.result.metadata == metadata)
}

impl Bot {
    /// `bot.recipesFor(itemType, metadata, minResultCount, craftingTable)`
    /// — unlocked recipes whose result is `kind` (metadata filter per
    /// upstream) and craftable `minResultCount` times with the CURRENT
    /// inventory. `crafting_table: Some(_)` means a table is usable.
    ///
    /// Recipes come from the server's recipe book (see module docs) —
    /// unlike upstream's static table this only covers unlocked
    /// recipes.
    pub fn recipes_for(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        min_result_count: Option<i32>,
        crafting_table: Option<&Block>,
    ) -> Vec<Recipe> {
        let min = min_result_count.unwrap_or(1);
        let has_table = crafting_table.is_some();
        let inventory = self.inventory();
        let counts = inventory_counts(&inventory);
        self.book_recipes()
            .filter(|r| result_matches(r, kind, metadata))
            .filter(|r| r.requirements_met(&mut counts.clone(), min, has_table))
            .collect()
    }

    /// `bot.recipesAll(itemType, metadata, craftingTable)` — unlocked
    /// recipes whose result is `kind`, ignoring inventory contents.
    pub fn recipes_all(
        &self,
        kind: ItemKind,
        metadata: Option<i32>,
        crafting_table: Option<&Block>,
    ) -> Vec<Recipe> {
        let has_table = crafting_table.is_some();
        self.book_recipes()
            .filter(|r| result_matches(r, kind, metadata))
            .filter(|r| !r.requires_table || has_table)
            .collect()
    }

    /// How many recipe-book entries the server has unlocked for us.
    /// Diagnostics — upstream has no book at all.
    #[doc(hidden)]
    pub fn known_recipe_count(&self) -> usize {
        self.shared.recipes.lock().entries.len()
    }

    /// Convert the recipe book into [`Recipe`]s (skipping
    /// furnace/smithing/stonecutter displays).
    fn book_recipes(&self) -> impl Iterator<Item = Recipe> {
        self.shared
            .recipes
            .lock()
            .entries
            .iter()
            .filter_map(recipe_from_entry)
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// `bot.craft(recipe, count, craftingTable)` — craft `count` (null
    /// → 1) times. `crafting_table` is the table `Block` to right-click
    /// open; `None` crafts on the 2x2 inventory grid. Mirrors
    /// `craft.js`: place one item per ingredient cell, put the rest of
    /// the held stack back, take the result, clear leftover grid cells.
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// // planks on the 2x2 inventory grid: no crafting table needed
    /// if let Some(recipe) = bot
    ///     .recipes_for(ItemKind::OakPlanks, None, None, None)
    ///     .first()
    /// {
    ///     bot.craft(recipe, Some(1), None).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn craft(
        &self,
        recipe: &Recipe,
        count: Option<i32>,
        crafting_table: Option<&Block>,
    ) -> Result<()> {
        if recipe.requires_table && crafting_table.is_none() {
            return Err(Error::InvalidArgument(format!(
                "Recipe requires craftingTable, but one was not supplied: {recipe:?}"
            )));
        }
        let mut window_table = None;
        let result = async {
            for _ in 0..count.unwrap_or(1).max(0) {
                self.craft_once(recipe, crafting_table, &mut window_table)
                    .await?;
            }
            Ok(())
        }
        .await;
        // upstream closes the table window whether the craft succeeded
        // or errored — but on success it first forces a slot resync
        // (`_syncWindow`): the last put-away click is unconfirmed, and
        // closing on a wrong model bakes phantom items into the
        // persisted inventory view
        if let Some(window) = window_table {
            if result.is_ok() {
                self.sync_window(&window).await;
            }
            self.close_window(&window);
        }
        result
    }

    /// One `craftOnce` iteration — place the ingredients, take the
    /// result, clear the grid.
    async fn craft_once(
        &self,
        recipe: &Recipe,
        crafting_table: Option<&Block>,
        window_table: &mut Option<Window>,
    ) -> Result<()> {
        let (window, w, h) = match crafting_table {
            Some(block) => {
                if window_table.is_none() {
                    // upstream activateBlock + once('windowOpen')
                    *window_table = Some(self.open_block(block).await?);
                }
                let window = window_table.clone().unwrap();
                if window.menu_kind() != Some(MenuKind::Crafting) {
                    return Err(Error::InvalidArgument(format!(
                        "crafting: non craftingTable used as craftingTable: {:?}",
                        window.menu_kind()
                    )));
                }
                (window, 3usize, 3usize)
            }
            None => (self.inventory(), 2, 2),
        };

        let mut original_source_slot = None;
        let mut extra_slots = unused_recipe_slots(recipe, w, h);

        if let Some(shape) = &recipe.in_shape {
            for (y, row) in shape.iter().enumerate() {
                for (x, cell) in row.iter().enumerate() {
                    let Some(ing) = cell else { continue };
                    self.click_ingredient(
                        &window,
                        ing,
                        grid_slot(x, y, w),
                        &mut original_source_slot,
                    )
                    .await?;
                }
            }
        }
        if let Some(ings) = &recipe.ingredients {
            for ing in ings {
                let dest = extra_slots.pop().ok_or_else(|| {
                    Error::NotFound("craft: no free grid slot for ingredient".into())
                })?;
                self.click_ingredient(&window, ing, dest, &mut original_source_slot)
                    .await?;
            }
        }

        // putMaterialsAway — return the rest of the held stack to the
        // player region, then grabResult (the server keeps the result
        // in slot 0; `put_away` takes it).
        let player = window.player_range();
        self.put_selected_item_range(
            player.start as i32,
            player.end as i32,
            &window,
            original_source_slot,
        )
        .await?;
        self.put_away(0).await?;

        // updateOutShape — upstream walks recipe.outShape; the live
        // grid scan covers it (and anything it would have missed).
        for i in 1..=(w * h) as i32 {
            if window.slot(i).is_some() {
                self.put_away(i as u16).await?;
            }
        }
        Ok(())
    }

    /// `clickShape`/`nextIngredientsClick` — pick up a matching stack
    /// unless the cursor already holds one, then right-click one item
    /// into `dest_slot`.
    async fn click_ingredient(
        &self,
        window: &Window,
        ingredient: &Ingredient,
        dest_slot: i32,
        original_source_slot: &mut Option<i32>,
    ) -> Result<()> {
        if !window
            .selected_item()
            .is_some_and(|held| ingredient.accepts(&held))
        {
            let source = window
                .inventory_items()
                .into_iter()
                .find(|item| ingredient.accepts(item))
                .ok_or_else(|| Error::NotFound("missing ingredient".into()))?;
            original_source_slot.get_or_insert(i32::from(source.slot));
            click_window_on(window, i32::from(source.slot), 0, 0, self.subscribe()).await?;
        }
        click_window_on(window, dest_slot, 1, 0, self.subscribe()).await
    }
}

/// `slot(x, y)` — grid cell to absolute window slot (slot 0 = result).
fn grid_slot(x: usize, y: usize, w: usize) -> i32 {
    (1 + x + w * y) as i32
}

/// `unusedRecipeSlots` — grid slots the shape doesn't occupy, in
/// upstream's scan order (`pop()` consumes from the end).
fn unused_recipe_slots(recipe: &Recipe, w: usize, h: usize) -> Vec<i32> {
    let mut result = Vec::new();
    match &recipe.in_shape {
        Some(shape) => {
            for (y, row) in shape.iter().enumerate() {
                for (x, cell) in row.iter().enumerate() {
                    if cell.is_none() {
                        result.push(grid_slot(x, y, w));
                    }
                }
                for x in row.len()..w {
                    result.push(grid_slot(x, y, w));
                }
            }
            for y in shape.len()..h {
                for x in 0..w {
                    result.push(grid_slot(x, y, w));
                }
            }
        }
        None => {
            for y in 0..h {
                for x in 0..w {
                    result.push(grid_slot(x, y, w));
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
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
}
