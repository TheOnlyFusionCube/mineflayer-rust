//! The mineflayer-tool plugin surface: `bot.tool()` — pick the right
//! hotbar item for a block or a fight.
//!
//! Block selection wraps azalea's auto-tool ([`Client::best_tool_in_hotbar_for_block`]),
//! the same scoring `mine_with_auto_tool` uses — item dig speed per block
//! state, including physics/effect modifiers, with the empty hand as a
//! candidate. Entity selection has no azalea analog: ours equips the
//! hotbar item with the highest melee [`attack_damage`] (vanilla's base
//! damage before enchant/cooldown factors) — upstream's weapon pick does
//! the same from mcdata; entity-specific scoring exists on neither side
//! (see api-parity for the `equipForEntity` note).
//!
//! Both are hotbar-only, like upstream's `equipForBlock`/`equipForEntity`
//! (upstream's `equipForBlock` does search the whole inventory, but only
//! to reject non-hotbar hits — `diverges` row).

use azalea::{
    core::attribute_modifier_operation::AttributeModifierOperation,
    entity::inventory::Inventory,
    inventory::{
        ItemStack, Menu,
        components::{AttributeModifiers, EquipmentSlotGroup},
    },
    registry::builtin::Attribute,
};

use crate::{Bot, Result, inventory::QUICK_BAR_START, windows::Item, world::Block};

/// Melee damage an item deals held in the main hand — 1 (the empty
/// hand's base) plus the item's `attack_damage` attribute modifiers in
/// the hand-applicable slot groups, folded the way vanilla folds them
/// (`+add`, `×(1+mult_base)`, `×Πmult_total`). The hand itself always
/// scores 1.
fn attack_damage(stack: &ItemStack) -> f64 {
    let Some(mods) = stack.get_component::<AttributeModifiers>() else {
        return 1.;
    };
    let (mut add, mut mul_base, mut mul_total) = (0., 0., 1.);
    for entry in &mods.modifiers {
        if entry.kind != Attribute::AttackDamage
            || !matches!(
                entry.slot,
                EquipmentSlotGroup::Any | EquipmentSlotGroup::Mainhand | EquipmentSlotGroup::Hand
            )
        {
            continue;
        }
        match entry.modifier.operation {
            AttributeModifierOperation::AddValue => add += entry.modifier.amount,
            AttributeModifierOperation::AddMultipliedBase => mul_base += entry.modifier.amount,
            AttributeModifierOperation::AddMultipliedTotal => {
                mul_total *= 1. + entry.modifier.amount;
            }
        }
    }
    (1. + add) * (1. + mul_base) * mul_total
}

/// The hotbar index (0–8) of the highest-damage item — first strictly-
/// better slot wins, so ties keep the earlier slot and an all-empty
/// hotbar returns 0.
fn best_melee_slot(menu: &Menu) -> usize {
    let slots = menu.slots();
    let mut best = 0;
    let mut best_damage = f64::NEG_INFINITY;
    for (i, stack) in slots[menu.hotbar_slots_range()].iter().enumerate() {
        let damage = attack_damage(stack);
        if damage > best_damage {
            best = i;
            best_damage = damage;
        }
    }
    best
}

/// The `bot.tool` namespace — a borrowed view onto the [`Bot`], like
/// [`Bot::pathfinder`].
pub struct Tool<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.tool` — the mineflayer-tool namespace (`equipForBlock`,
    /// `equipForEntity`, `bestToolForBlock`).
    pub fn tool(&self) -> Tool<'_> {
        Tool { bot: self }
    }
}

impl Tool<'_> {
    /// `bot.tool.equipForBlock(block)` — select the hotbar slot that digs
    /// `block` fastest (azalea's score covers the empty hand too, so the
    /// hand wins for blocks no tool speeds up).
    pub async fn equip_for_block(&self, block: &Block) -> Result<()> {
        let best = self.bot.client.best_tool_in_hotbar_for_block(block.state);
        self.bot.set_quick_bar_slot(best.index as u8).await
    }

    /// `bot.tool.bestToolForBlock`-analog — the item `equip_for_block`
    /// would select, or `None` when the empty hand is the best "tool"
    /// (azalea rates empty slots; upstream returns the item or nothing).
    pub fn best_tool_for(&self, block: &Block) -> Option<Item> {
        let best = self.bot.client.best_tool_in_hotbar_for_block(block.state);
        self.hotbar_item(best.index)
    }

    /// `bot.tool.equipForEntity(entity)` — equip the strongest melee
    /// weapon on the hotbar. The entity's own attributes can't be
    /// consulted (azalea's best-tool is block-oriented, and no
    /// entity-aware weapon scoring exists), so this is a generic
    /// max-damage pick — see the `equipForEntity` row in api-parity.md.
    pub async fn equip_for_entity(&self, _entity: &azalea::EntityRef) -> Result<()> {
        let index = self
            .bot
            .client
            .get_component::<Inventory>()
            .map(|inv| best_melee_slot(&inv.inventory_menu))
            .unwrap_or(0);
        self.bot.set_quick_bar_slot(index as u8).await
    }

    /// The [`Item`] in hotbar index `i` (absolute player-window slot
    /// `36+i`), `None` when the slot is empty — read from the same
    /// `Inventory` component the best-tool scoring uses.
    fn hotbar_item(&self, index: usize) -> Option<Item> {
        let inv = self.bot.client.get_component::<Inventory>()?;
        let stack = inv.inventory_menu.slot(QUICK_BAR_START + index).cloned()?;
        Item::of(stack, (QUICK_BAR_START + index) as i16)
    }
}

#[cfg(test)]
mod tests {
    use azalea::{
        BlockPos,
        block::{BlockState, fluid_state::FluidKind},
        entity::{ActiveEffects, Attributes, FluidOnEyes, Physics, inventory::Inventory},
        registry::builtin::{BlockKind, EntityKind, ItemKind},
    };

    use super::*;
    use crate::{test_util::live_entity_client, world::Block};

    /// The components `best_tool_in_hotbar_for_block` queries plus the
    /// inventory being scored, with the given (hotbar slot → stack)
    /// entries set.
    fn player_with_hotbar(items: &[(usize, ItemStack)]) -> azalea::Client {
        let mut inventory = Inventory::default();
        for (slot, stack) in items {
            *inventory
                .inventory_menu
                .slot_mut(QUICK_BAR_START + slot)
                .unwrap() = stack.clone();
        }
        live_entity_client((
            inventory,
            Physics::default(),
            FluidOnEyes::new(FluidKind::Empty),
            Attributes::new(EntityKind::Player),
            ActiveEffects::default(),
        ))
    }

    #[test]
    fn attack_damage_scores_weapons_above_the_bare_hand() {
        // diamond sword: base 1 + the registry's +6 attack_damage
        assert_eq!(
            attack_damage(&ItemStack::new(ItemKind::DiamondSword, 1)),
            7.
        );
        // a stick has no attribute modifiers; the hand itself scores 1
        assert_eq!(attack_damage(&ItemStack::new(ItemKind::Stick, 1)), 1.);
        assert_eq!(attack_damage(&ItemStack::Empty), 1.);
        assert!(
            attack_damage(&ItemStack::new(ItemKind::DiamondSword, 1))
                > attack_damage(&ItemStack::new(ItemKind::DiamondShovel, 1))
        );
    }

    #[test]
    fn best_melee_slot_picks_the_strongest_hotbar_item() {
        let mut menu = Inventory::default().inventory_menu;
        *menu.slot_mut(QUICK_BAR_START).unwrap() = ItemStack::new(ItemKind::Stick, 1);
        *menu.slot_mut(QUICK_BAR_START + 4).unwrap() = ItemStack::new(ItemKind::DiamondSword, 1);
        assert_eq!(best_melee_slot(&menu), 4);

        // an all-empty hotbar ties at damage 1 everywhere → slot 0
        assert_eq!(best_melee_slot(&Inventory::default().inventory_menu), 0);

        // ties keep the earlier slot
        let mut tied = Inventory::default().inventory_menu;
        *tied.slot_mut(QUICK_BAR_START + 2).unwrap() = ItemStack::new(ItemKind::DiamondSword, 1);
        *tied.slot_mut(QUICK_BAR_START + 6).unwrap() = ItemStack::new(ItemKind::DiamondSword, 1);
        assert_eq!(best_melee_slot(&tied), 2);
    }

    #[test]
    fn best_tool_for_returns_the_digging_winner() {
        let bot = Bot::new(player_with_hotbar(&[
            (0, ItemStack::new(ItemKind::DiamondSword, 1)),
            (1, ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        ]));
        let stone = Block::new(
            &bot.client,
            BlockPos::new(0, 64, 0),
            BlockState::from(BlockKind::Stone),
        );
        let best = bot.tool().best_tool_for(&stone).expect("a tool wins");
        assert_eq!(best.kind(), ItemKind::DiamondPickaxe);
        assert_eq!(best.slot, (QUICK_BAR_START + 1) as i16);

        // an empty hotbar has nothing better than the bare hand → None
        let bare = Bot::new(player_with_hotbar(&[]));
        assert!(bare.tool().best_tool_for(&stone).is_none());
    }

    #[tokio::test]
    async fn equip_for_block_selects_the_best_slot() {
        let bot = Bot::new(player_with_hotbar(&[
            (0, ItemStack::new(ItemKind::DiamondSword, 1)),
            (3, ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        ]));
        let stone = Block::new(
            &bot.client,
            BlockPos::new(0, 64, 0),
            BlockState::from(BlockKind::Stone),
        );
        bot.tool().equip_for_block(&stone).await.unwrap();
        assert_eq!(bot.quick_bar_slot(), 3);
    }

    #[tokio::test]
    async fn equip_for_entity_selects_the_best_melee_slot() {
        let client = player_with_hotbar(&[
            (0, ItemStack::new(ItemKind::Stick, 1)),
            (2, ItemStack::new(ItemKind::DiamondSword, 1)),
        ]);
        let bot = Bot::new(client.clone());
        let target = client.entity_ref_for(client.entity); // arg is unused
        bot.tool().equip_for_entity(&target).await.unwrap();
        assert_eq!(bot.quick_bar_slot(), 2);
    }
}
