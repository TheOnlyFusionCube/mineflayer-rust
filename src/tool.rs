//! The mineflayer-tool plugin surface: `bot.tool()` — pick the right
//! item for a block or a fight.
//!
//! Block selection scans the whole player inventory like upstream's
//! `equipForBlock` (`bot.inventory.items()` + a bare-hand candidate
//! when a free slot exists), scoring each stack with the engine's
//! `get_mine_progress` — item dig speed per block state including
//! physics/effect modifiers, the same scoring `mine_with_auto_tool`
//! uses. Entity selection has no engine analog: ours equips the hotbar
//! item with the highest melee `attack_damage` (vanilla's base damage
//! before enchant/cooldown factors) — upstream's weapon pick does the
//! same from mcdata; entity-specific scoring exists on neither side
//! (see api-parity for the `equipForEntity` note).

use std::time::Duration;

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// real crate name in scope.
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    BlockPos,
    block::BlockTrait,
    core::attribute_modifier_operation::AttributeModifierOperation,
    ecs::prelude::Component,
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, inventory::Inventory,
        mining::get_mine_progress,
    },
    inventory::{
        ItemStack,
        components::{AttributeModifiers, EquipmentSlotGroup, Tool as ToolComponent},
    },
    registry::builtin::Attribute,
};

use crate::bot::{with_component, with_component_mut};
use crate::{
    Bot, Error, Result,
    inventory::{EquipDest, QUICK_BAR_START},
    pathfinder::Goal,
    windows::Item,
    world::Block,
};

/// Bound a chest trip like collect's per-candidate walk — upstream
/// waits on `goto` forever, but a stalled partial path shouldn't hang
/// a `getFromChest` retry.
const CHEST_GOTO_TIMEOUT: Duration = Duration::from_secs(30);

/// Upstream `MiningEquipOptions` — the options bag for
/// [`Tool::equip_for_block_opts`].
#[derive(Clone, Default)]
pub struct EquipOpts {
    /// `requireHarvest` — keep only candidates that can harvest the
    /// block (produce drops). With none available, `getFromChest` runs
    /// first and the call errors `NoItem` when retrieval can't help
    /// (default false upstream).
    pub require_harvest: bool,
    /// `getFromChest` — when no inventory candidate works, pull tools
    /// from [`Tool::chest_locations`] nearest-first and retry (default
    /// false upstream).
    pub get_from_chest: bool,
    /// `maxTools` — the most stacks to pull per chest retrieval
    /// (upstream `?? 1`).
    pub max_tools: Option<usize>,
}

/// Facade state the engine has no slot for — upstream's
/// `Tool.chestLocations` array.
/// Stored as an ECS component created on first use — rule 2 of the
/// storage rule on `bot::BotShared`.
#[derive(Component, Default)]
struct ToolState {
    /// `chestLocations` — positions of chests `getFromChest` may pull
    /// tools from.
    chest_locations: Vec<BlockPos>,
}

/// `block.canHarvest(itemType)` — whether `stack` produces drops
/// breaking `state`. Mirrors mineflayer-entity's private
/// `has_correct_tool_for_drops` (the mc1.20.5+ tool-component rule
/// check upstream reads from mcdata `harvestTools`): blocks that don't
/// require a correct tool harvest with anything, bare hand included.
pub(crate) fn can_harvest(state: mineflayer_engine::block::BlockState, stack: &ItemStack) -> bool {
    let block = Box::<dyn BlockTrait>::from(state);
    if !block.behavior().requires_correct_tool_for_drops {
        return true;
    }
    let Some(tool) = stack.get_component::<ToolComponent>() else {
        return false;
    };
    let kind = block.as_block_kind();
    for rule in &tool.rules {
        if let Some(correct) = rule.correct_for_drops
            && rule.blocks.contains(kind)
        {
            return correct;
        }
    }
    false
}

/// `getDigTime` analog — mine progress per tick for `stack` on `state`
/// (upstream sorts candidates by dig time ascending; progress per tick
/// is its monotone inverse, so we sort descending).
#[allow(clippy::too_many_arguments)]
fn dig_progress(
    state: mineflayer_engine::block::BlockState,
    stack: &ItemStack,
    fluid_on_eyes: &FluidOnEyes,
    physics: &Physics,
    attributes: &Attributes,
    active_effects: &ActiveEffects,
) -> f32 {
    let block = Box::<dyn BlockTrait>::from(state);
    get_mine_progress(
        block.as_ref(),
        stack,
        fluid_on_eyes,
        physics,
        attributes,
        active_effects,
    )
}

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
fn best_melee_slot(menu: &mineflayer_engine::inventory::Menu) -> usize {
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

/// What [`Tool::pick`] decided — upstream `equipForBlock`'s branches.
enum ToolPick {
    /// `itemList` was empty — run the `getFromChest`/`NoItem` path.
    Empty,
    /// `isBetterMiningTool` was false — the held item is already the
    /// joint-best candidate; upstream does nothing here.
    Skip,
    /// `equip(best, 'hand')` — this inventory stack won.
    Item(Item),
    /// `unequip('hand')` — the bare hand won (only a candidate when a
    /// free inventory slot exists to receive the held stack).
    Hand,
}

/// The `bot.tool` namespace — a borrowed view onto the [`Bot`], like
/// [`Bot::pathfinder`].
pub struct Tool<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.tool` — the mineflayer-tool namespace (`equipForBlock`,
    /// `equipForEntity`, `chestLocations`).
    pub fn tool(&self) -> Tool<'_> {
        Tool { bot: self }
    }
}

impl Tool<'_> {
    /// `bot.tool.chestLocations` — the chest positions `getFromChest`
    /// may pull tools from (upstream's public array).
    pub fn chest_locations(&self) -> Vec<BlockPos> {
        with_component::<ToolState, _>(&self.bot.client, |s| s.chest_locations.clone())
            .unwrap_or_default()
    }

    /// Replace `chestLocations`.
    pub fn set_chest_locations(&self, locations: Vec<BlockPos>) {
        with_component_mut::<ToolState, _>(&self.bot.client, |s| s.chest_locations = locations);
    }

    /// `chestLocations.push(pos)` — upstream mutates the array
    /// directly; same effect.
    pub fn add_chest_location(&self, pos: BlockPos) {
        with_component_mut::<ToolState, _>(&self.bot.client, |s| s.chest_locations.push(pos));
    }

    /// `bot.tool.equipForBlock(block)` — the default-options call.
    pub async fn equip_for_block(&self, block: &Block) -> Result<()> {
        self.equip_for_block_opts(block, &EquipOpts::default())
            .await
    }

    /// `bot.tool.equipForBlock(block, options)` — pick the fastest
    /// harvest candidate across the whole inventory and hold it.
    ///
    /// Candidates are every non-empty stack in the 36 player-inventory
    /// slots, plus the bare hand when a free slot exists for the held
    /// stack (upstream's `unshift(undefined)`); `require_harvest` keeps
    /// only stacks that can produce drops on `block`. An empty list
    /// either triggers `get_from_chest` retrieval + retry or returns
    /// the upstream `NoItem` error when `require_harvest` is set.
    /// Already holding a joint-best tool is a no-op, like upstream's
    /// `isBetterMiningTool`.
    ///
    /// # Errors
    ///
    /// [`Error::NotConnected`] before the bot has spawned;
    /// [`Error::NotFound`] `NoItem`/`NoChest`/`UnloadedChunk` on the
    /// upstream failure paths, and whatever `equip`/`open_chest`/`goto`
    /// fails with underneath.
    pub async fn equip_for_block_opts(&self, block: &Block, opts: &EquipOpts) -> Result<()> {
        match self.pick(block, opts.require_harvest)? {
            ToolPick::Skip => Ok(()),
            ToolPick::Item(item) => self.bot.equip(&item, EquipDest::Hand).await,
            ToolPick::Hand => self.bot.unequip(EquipDest::Hand).await,
            ToolPick::Empty => {
                if opts.get_from_chest {
                    self.retrieve_tools(block, opts.max_tools.unwrap_or(1))
                        .await?;
                    // upstream recurses with the same options — each
                    // retrieval depletes a chest, so `NoChest` bounds it
                    return Box::pin(self.equip_for_block_opts(block, opts)).await;
                }
                if opts.require_harvest {
                    return Err(Error::NotFound(
                        "NoItem: Bot does not have a harvestable tool!".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    /// `bot.tool.bestToolForBlock`-analog — the item `equip_for_block`
    /// would select, or `None` when the bare hand is the best "tool".
    pub fn best_tool_for(&self, block: &Block) -> Option<Item> {
        match self.pick(block, false).ok()? {
            ToolPick::Item(item) => Some(item),
            _ => None,
        }
    }

    /// The upstream candidate scan — sorted picks as [`ToolPick`].
    /// `Err(NotConnected)` when the player entity is gone.
    fn pick(&self, block: &Block, require_harvest: bool) -> Result<ToolPick> {
        let state = block.state;
        let picked = self.bot.client.try_query_self::<(
            &Inventory,
            &Physics,
            &FluidOnEyes,
            &Attributes,
            &ActiveEffects,
        ), _>(
            |(inventory, physics, fluid_on_eyes, attributes, active_effects)| {
                let menu = &inventory.inventory_menu;
                let slots = menu.slots();
                let player = menu.player_slots_range();
                let progress = |stack: &ItemStack| {
                    dig_progress(
                        state,
                        stack,
                        fluid_on_eyes,
                        physics,
                        attributes,
                        active_effects,
                    )
                };
                let harvest_ok = |stack: &ItemStack| !require_harvest || can_harvest(state, stack);
                // upstream `unshift(undefined)`: the bare hand is a
                // candidate only when a slot is free for the held stack to
                // unequip into — count empty slots in the player region
                let has_free_slot = slots[player.clone()].iter().any(|s| s.is_empty());
                let mut best: Option<(f32, Option<Item>)> = None;
                let consider = |score_slot: i16, stack: &ItemStack, best: &mut _| {
                    if !harvest_ok(stack) {
                        return;
                    }
                    let this = progress(stack);
                    let better = match best {
                        None => true,
                        Some((top, _)) => this > *top,
                    };
                    if better {
                        *best = Some((
                            this,
                            if stack.is_empty() {
                                None
                            } else {
                                Some(Item {
                                    stack: stack.clone(),
                                    slot: score_slot,
                                })
                            },
                        ));
                    }
                };
                if has_free_slot {
                    consider(-1, &ItemStack::Empty, &mut best);
                }
                for i in player.clone() {
                    if let stack @ ItemStack::Present(_) = &slots[i] {
                        consider(i as i16, stack, &mut best);
                    }
                }
                // `isBetterMiningTool` — a held item that survives the
                // filter and ties/wins against the pick is left alone
                let held_slot = QUICK_BAR_START + usize::from(inventory.selected_hotbar_slot);
                let held = &slots[held_slot];
                if held.is_present()
                    && harvest_ok(held)
                    && let Some((top, _)) = &best
                    && *top <= progress(held)
                {
                    return ToolPick::Skip;
                }
                match best {
                    None => ToolPick::Empty,
                    Some((_, item)) => match item {
                        Some(item) => ToolPick::Item(item),
                        None => ToolPick::Hand,
                    },
                }
            },
        );
        picked.map_err(|_| Error::NotConnected)
    }

    /// `retrieveTools`/`pullFromChest` — visit `chest_locations`
    /// nearest-first, withdraw up to `max_tools` stacks matching
    /// upstream's `standardToolFilter`, best-dig-time first, and stop
    /// at the first chest that yielded anything.
    async fn retrieve_tools(&self, block: &Block, max_tools: usize) -> Result<()> {
        let bot = self.bot;
        let mut locations = self.chest_locations();
        while !locations.is_empty() {
            let my = bot.position();
            let Some((nearest, _)) = locations
                .iter()
                .enumerate()
                .map(|(i, p)| (i, my.distance_to(p.center())))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            else {
                break;
            };
            let chest_pos = locations.remove(nearest);
            let pf = bot.pathfinder();
            tokio::time::timeout(CHEST_GOTO_TIMEOUT, pf.goto(Goal::GetToBlock(chest_pos)))
                .await
                .map_err(|_| {
                    Error::Failed(format!(
                        "path to {chest_pos} timed out after {CHEST_GOTO_TIMEOUT:?}"
                    ))
                })??;
            let Some(chest_block) = bot.block_at(chest_pos) else {
                return Err(Error::NotFound(
                    "UnloadedChunk: Chest is located in an unloaded chunk!".into(),
                ));
            };
            let chest = bot.open_chest(&chest_block).await?;
            let mut pulls: Vec<Item> = chest
                .container_items()
                .into_iter()
                .filter(standard_tool_filter)
                .collect();
            if pulls.is_empty() {
                chest.close();
                continue;
            }
            // cheapest dig time first (upstream sorts toolCostFilter asc)
            pulls.sort_by(|a, b| {
                self.dig_progress_of(&b.stack, block)
                    .partial_cmp(&self.dig_progress_of(&a.stack, block))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for item in pulls.into_iter().take(max_tools) {
                chest
                    .withdraw(item.kind(), item.metadata(), Some(item.count()), None)
                    .await?;
            }
            chest.close();
            // upstream wait(200) — let the server's slot updates land
            // before the caller re-scans the inventory
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Ok(());
        }
        Err(Error::NotFound(
            "NoChest: There are no chests with available tools in them!".into(),
        ))
    }

    /// `get_mine_progress` for one stack outside the pick closure —
    /// `0.` when the player components are gone.
    fn dig_progress_of(&self, stack: &ItemStack, block: &Block) -> f32 {
        let state = block.state;
        self.bot
            .client
            .try_query_self::<(&Physics, &FluidOnEyes, &Attributes, &ActiveEffects), _>(
                |(physics, fluid_on_eyes, attributes, active_effects)| {
                    dig_progress(
                        state,
                        stack,
                        fluid_on_eyes,
                        physics,
                        attributes,
                        active_effects,
                    )
                },
            )
            .unwrap_or(0.)
    }

    /// `bot.tool.equipForEntity(entity)` — equip the strongest melee
    /// weapon on the hotbar. The entity's own attributes can't be
    /// consulted (the engine's best-tool is block-oriented, and no
    /// entity-aware weapon scoring exists), so this is a generic
    /// max-damage pick — see the `equipForEntity` row in api-parity.md.
    pub async fn equip_for_entity(&self, _entity: &mineflayer_engine::EntityRef) -> Result<()> {
        let index = self
            .bot
            .client
            .get_component::<Inventory>()
            .map(|inv| best_melee_slot(&inv.inventory_menu))
            .unwrap_or(0);
        self.bot.set_quick_bar_slot(index as u8).await
    }
}

/// Upstream `standardToolFilter` — true for anything name-matching a
/// tool or weapon (`'sword'`, `'pickaxe'`, `'shovel'`, `'axe'`,
/// `'hoe'`).
fn standard_tool_filter(item: &Item) -> bool {
    let name = item.name();
    ["sword", "pickaxe", "shovel", "axe", "hoe"]
        .iter()
        .any(|t| name.contains(t))
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{
        BlockPos,
        block::{BlockState, fluid_state::FluidKind},
        entity::{ActiveEffects, Attributes, FluidOnEyes, Physics, inventory::Inventory},
        registry::builtin::{BlockKind, EntityKind, ItemKind},
        registry::identifier::Identifier,
        world::WorldName,
    };

    use super::*;
    use crate::{test_util::live_entity_client, world::Block};

    /// The components `get_mine_progress` queries plus the inventory
    /// being scored, with the given (absolute player-window slot →
    /// stack) entries set — hotbar slots are 36–44, main inventory
    /// 9–35.
    fn player_with_inventory(items: &[(usize, ItemStack)]) -> mineflayer_engine::Client {
        let mut inventory = Inventory::default();
        for (slot, stack) in items {
            *inventory.inventory_menu.slot_mut(*slot).unwrap() = stack.clone();
        }
        live_entity_client((
            inventory,
            WorldName(Identifier::from("minecraft:overworld")),
            Physics::default(),
            FluidOnEyes::new(FluidKind::Empty),
            Attributes::new(EntityKind::Player),
            ActiveEffects::default(),
        ))
    }

    /// Hotbar-slot shorthand — `player_with_inventory(&[(QUICK_BAR_START + i, s)])`.
    fn player_with_hotbar(items: &[(usize, ItemStack)]) -> mineflayer_engine::Client {
        player_with_inventory(
            &items
                .iter()
                .map(|(i, s)| (QUICK_BAR_START + i, s.clone()))
                .collect::<Vec<_>>(),
        )
    }

    fn stone(bot: &Bot) -> Block {
        Block::new(
            &bot.client,
            BlockPos::new(0, 64, 0),
            BlockState::from(BlockKind::Stone),
        )
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
    fn can_harvest_gates_on_requires_correct_tool() {
        // stone drops nothing without a pickaxe; dirt harvests with anything
        let stone = BlockState::from(BlockKind::Stone);
        let dirt = BlockState::from(BlockKind::Dirt);
        let pick = ItemStack::new(ItemKind::DiamondPickaxe, 1);
        assert!(can_harvest(stone, &pick));
        assert!(!can_harvest(stone, &ItemStack::Empty));
        assert!(!can_harvest(stone, &ItemStack::new(ItemKind::Stick, 1)));
        assert!(can_harvest(dirt, &ItemStack::Empty));
        assert!(can_harvest(dirt, &ItemStack::new(ItemKind::Stick, 1)));
    }

    #[test]
    fn pick_scans_the_whole_inventory_like_upstream() {
        // the best tool sits in the main inventory, not the hotbar —
        // upstream's items() scan reaches it
        let bot = crate::test_util::bot(player_with_inventory(&[
            (
                QUICK_BAR_START + 1,
                ItemStack::new(ItemKind::DiamondSword, 1),
            ),
            (12, ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        ]));
        match bot.tool().pick(&stone(&bot), false).unwrap() {
            ToolPick::Item(item) => {
                assert_eq!(item.kind(), ItemKind::DiamondPickaxe);
                assert_eq!(item.slot, 12);
            }
            _ => panic!("a main-inventory pickaxe must be found"),
        }
    }

    #[test]
    fn pick_require_harvest_filters_and_errors() {
        // no harvestable tool anywhere → Empty (the NoItem path)
        let bare = crate::test_util::bot(player_with_hotbar(&[]));
        assert!(matches!(
            bare.tool().pick(&stone(&bare), true).unwrap(),
            ToolPick::Empty
        ));

        // hand can't harvest stone; a dirt-block check leaves it a candidate
        let dirt = Block::new(
            &bare.client,
            BlockPos::new(0, 64, 0),
            BlockState::from(BlockKind::Dirt),
        );
        assert!(matches!(
            bare.tool().pick(&dirt, true).unwrap(),
            ToolPick::Hand
        ));

        // a pickaxe survives the filter (main inventory, so it isn't
        // the already-held item — that case is `Skip`)
        let bot = crate::test_util::bot(player_with_inventory(&[(
            12,
            ItemStack::new(ItemKind::DiamondPickaxe, 1),
        )]));
        assert!(matches!(
            bot.tool().pick(&stone(&bot), true).unwrap(),
            ToolPick::Item(_)
        ));
    }

    #[test]
    fn pick_skips_when_the_held_item_is_already_best() {
        let bot = crate::test_util::bot(player_with_hotbar(&[(
            0,
            ItemStack::new(ItemKind::DiamondPickaxe, 1),
        )]));
        assert_eq!(bot.quick_bar_slot(), 0);
        assert!(matches!(
            bot.tool().pick(&stone(&bot), false).unwrap(),
            ToolPick::Skip
        ));
    }

    #[test]
    fn chest_locations_round_trip() {
        let bot = crate::test_util::bot(player_with_hotbar(&[]));
        assert!(bot.tool().chest_locations().is_empty());
        bot.tool().set_chest_locations(vec![BlockPos::new(1, 2, 3)]);
        bot.tool().add_chest_location(BlockPos::new(4, 5, 6));
        assert_eq!(
            bot.tool().chest_locations(),
            vec![BlockPos::new(1, 2, 3), BlockPos::new(4, 5, 6)]
        );
    }

    #[test]
    fn standard_tool_filter_matches_upstream_names() {
        let pick = Item {
            stack: ItemStack::new(ItemKind::DiamondPickaxe, 1),
            slot: 0,
        };
        let stone_item = Item {
            stack: ItemStack::new(ItemKind::Stone, 1),
            slot: 0,
        };
        assert!(standard_tool_filter(&pick));
        assert!(!standard_tool_filter(&stone_item));
    }

    #[test]
    fn best_tool_for_returns_the_inventory_winner() {
        let bot = crate::test_util::bot(player_with_inventory(&[
            (QUICK_BAR_START, ItemStack::new(ItemKind::DiamondSword, 1)),
            (20, ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        ]));
        let best = bot.tool().best_tool_for(&stone(&bot)).expect("a tool wins");
        assert_eq!(best.kind(), ItemKind::DiamondPickaxe);
        assert_eq!(best.slot, 20);

        // an empty hotbar+inventory has nothing better than the bare hand
        let bare = crate::test_util::bot(player_with_hotbar(&[]));
        assert!(bare.tool().best_tool_for(&stone(&bare)).is_none());
    }

    #[tokio::test]
    async fn equip_for_block_selects_the_best_slot() {
        let bot = crate::test_util::bot(player_with_hotbar(&[
            (0, ItemStack::new(ItemKind::DiamondSword, 1)),
            (3, ItemStack::new(ItemKind::DiamondPickaxe, 1)),
        ]));
        bot.tool().equip_for_block(&stone(&bot)).await.unwrap();
        assert_eq!(bot.quick_bar_slot(), 3);
    }

    #[tokio::test]
    async fn equip_for_entity_selects_the_best_melee_slot() {
        let client = player_with_hotbar(&[
            (0, ItemStack::new(ItemKind::Stick, 1)),
            (2, ItemStack::new(ItemKind::DiamondSword, 1)),
        ]);
        let bot = crate::test_util::bot(client.clone());
        let target = client.entity_ref_for(client.entity); // arg is unused
        bot.tool().equip_for_entity(&target).await.unwrap();
        assert_eq!(bot.quick_bar_slot(), 2);
    }
}
