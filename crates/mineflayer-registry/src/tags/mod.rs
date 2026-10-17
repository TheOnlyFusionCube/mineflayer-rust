use std::{collections::HashSet, ops::Deref};

use crate::{Registry, builtin::ItemKind};

pub mod blocks;
pub mod entities;
pub mod fluids;
pub mod items;

/// The item tag in [`items`] with this name, without the `minecraft:`
/// namespace (`"logs"` is `minecraft:logs`).
///
/// Written by hand next to the generated statics; the test
/// `every_item_tag_is_found_by_its_name` fails when a static is missing.
pub fn item_tag_by_name(name: &str) -> Option<&'static RegistryTag<ItemKind>> {
    Some(match name {
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

/// A set of registry items.
#[derive(Clone, Debug)]
pub struct RegistryTag<R: Registry + 'static> {
    // yes, this *could* be a Box<[R]>, but there are cases in which the user may want to cheaply
    // mutate this.
    // yes, it could also be a [R; N] using a const generic, but that makes RegistryTag annoying if
    // the user wants to put it in their own code.
    // yes, if the aforementioned issues are ignored then we could even avoid a LazyLock. but that
    // would provide nearly no benefit and result in wasted memory for unused tags.
    // having it be a vec is fine.
    entries: Vec<R>,
}
impl<R: Registry + 'static> RegistryTag<R> {
    pub(crate) fn new(entries: Vec<R>) -> Self {
        // must be sorted for binary search
        debug_assert!(entries.is_sorted());

        Self { entries }
    }
}
impl<R: Registry + 'static> RegistryTag<R> {
    /// Returns whether the given item is contained in this registry.
    pub fn contains(&self, value: &R) -> bool {
        self.find(value).is_some()
    }

    pub fn remove(&mut self, value: &R) -> Option<R> {
        self.find(value).map(|index| self.entries.remove(index))
    }

    fn find(&self, value: &R) -> Option<usize> {
        // TODO: tune this number; when does binary search actually start making
        // a difference?
        if self.entries.len() > 64 {
            self.binary_search_find(value)
        } else {
            self.linear_search_find(value)
        }
    }
    fn linear_search_find(&self, value: &R) -> Option<usize> {
        self.entries.iter().position(|e| e == value)
    }
    fn binary_search_find(&self, value: &R) -> Option<usize> {
        self.entries.binary_search(value).ok()
    }

    // this exists for convenience since we can't always do HashSet::from if
    // it's a LazyLock
    pub fn into_hashset(&self) -> HashSet<R> {
        self.clone().into_iter().collect()
    }
}
impl<R: Registry> IntoIterator for RegistryTag<R> {
    type Item = R;

    type IntoIter = std::vec::IntoIter<R>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<R: Registry> From<RegistryTag<R>> for HashSet<R> {
    fn from(tag: RegistryTag<R>) -> Self {
        tag.into_hashset()
    }
}

impl<R: Registry> Deref for RegistryTag<R> {
    type Target = [R];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl<R: Registry> FromIterator<R> for RegistryTag<R> {
    fn from_iter<T: IntoIterator<Item = R>>(iter: T) -> Self {
        let mut entries = iter.into_iter().collect::<Vec<_>>();
        entries.sort();
        Self::new(entries)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_item_tag_is_found_by_its_name() {
        let names: Vec<String> = include_str!("items.rs")
            .lines()
            .filter_map(|line| line.strip_prefix("pub static "))
            .filter_map(|rest| rest.split(':').next())
            .map(str::to_lowercase)
            .collect();
        assert!(!names.is_empty());
        let mut tags = HashSet::new();
        for name in &names {
            let tag = item_tag_by_name(name).unwrap_or_else(|| panic!("no item tag {name}"));
            assert!(tags.insert(std::ptr::from_ref(tag)), "{name} shares a tag");
        }
        assert!(item_tag_by_name("not_a_tag").is_none());
    }
}
