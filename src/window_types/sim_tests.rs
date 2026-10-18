//! The typed windows on the real engine (`BotSim`), with a test playing the
//! server: it opens the screen, sends the window data, and confirms each
//! click as vanilla does.

use mineflayer_engine::{
    BlockPos, FormattedText, Vec3,
    block::BlockState,
    inventory::ItemStack,
    protocol::packets::game::{
        ClientboundAddEntity, ClientboundBlockUpdate, ClientboundContainerSetContent,
        ClientboundContainerSetData, ClientboundMerchantOffers, ClientboundOpenScreen,
        ServerboundGamePacket,
        c_merchant_offers::{self, ItemCost, MerchantOffer},
    },
    registry::builtin::{BlockKind, EntityKind, ItemKind, MenuKind},
};

use crate::{Error, test_util::sim::BotSim, world::Block};

const WINDOW: i32 = 1;

/// A bot next to a block of `kind`.
fn next_to(kind: BlockKind) -> (BotSim, Block) {
    let mut sim = BotSim::new();
    let pos = BlockPos::from(sim.bot.position()).east(1);
    sim.receive(ClientboundBlockUpdate {
        pos,
        block_state: BlockState::from(kind),
    });
    sim.tick();
    let block = sim.bot.block_at(pos).expect("the block is loaded");
    (sim, block)
}

/// `size` container slots, then the 36 player slots, with `items` placed.
fn contents(size: usize, items: &[(usize, ItemStack)]) -> Vec<ItemStack> {
    let mut slots = vec![ItemStack::Empty; size + 36];
    for (slot, stack) in items {
        slots[*slot] = stack.clone();
    }
    slots
}

/// A server that opens a `kind` window holding `items` when the bot
/// uses a block, and confirms clicks.
fn opens(kind: MenuKind, items: Vec<ItemStack>) -> impl FnMut(&mut BotSim, &ServerboundGamePacket) {
    move |sim, packet| {
        if let ServerboundGamePacket::UseItemOn(_) = packet {
            sim.receive(ClientboundOpenScreen {
                container_id: WINDOW,
                menu_type: kind,
                title: FormattedText::from("window"),
            });
            sim.receive(ClientboundContainerSetContent {
                container_id: WINDOW,
                state_id: 1,
                items: items.clone(),
                carried_item: ItemStack::Empty,
            });
        }
        sim.echo_clicks(packet);
    }
}

fn set_data(sim: &mut BotSim, property: u16, value: u16) {
    sim.receive(ClientboundContainerSetData {
        container_id: WINDOW,
        id: property,
        value,
    });
}

#[test]
fn a_furnace_reads_its_data_and_moves_items() {
    let (mut sim, block) = next_to(BlockKind::Furnace);
    let items = contents(
        3,
        &[
            (0, ItemStack::new(ItemKind::IronOre, 4)),
            (2, ItemStack::new(ItemKind::IronIngot, 2)),
            (3 + 27, ItemStack::new(ItemKind::Coal, 8)),
        ],
    );
    let bot = sim.bot.clone();
    let furnace = sim
        .drive(bot.open_furnace(&block), opens(MenuKind::Furnace, items))
        .expect("the furnace opened");

    // fuel left 800 of 1600 ticks; 100 of 200 ticks cooked
    for (property, value) in [(0, 800), (1, 1600), (2, 100), (3, 200)] {
        set_data(&mut sim, property, value);
    }
    sim.tick();
    assert_eq!(furnace.fuel(), 0.5);
    assert_eq!(furnace.progress(), 0.5);
    assert_eq!(furnace.total_fuel(), Some(1600));
    assert_eq!(furnace.total_progress(), Some(200));
    assert_eq!(furnace.fuel_seconds(), 40.);
    assert_eq!(furnace.progress_seconds(), 5.);
    assert_eq!(furnace.total_fuel_seconds(), Some(80.));
    assert_eq!(furnace.total_progress_seconds(), Some(10.));
    assert_eq!(
        furnace.input_item().map(|i| i.kind()),
        Some(ItemKind::IronOre)
    );
    assert!(furnace.fuel_item().is_none());

    let output = sim
        .drive(furnace.take_output(), BotSim::echo_clicks)
        .expect("the output moved");
    assert_eq!(output.kind(), ItemKind::IronIngot);
    assert!(furnace.output_item().is_none());

    sim.drive(
        furnace.put_fuel(ItemKind::Coal, None, Some(3)),
        BotSim::echo_clicks,
    )
    .expect("the fuel moved");
    assert_eq!(furnace.fuel_item().map(|i| i.count()), Some(3));

    let input = sim
        .drive(furnace.take_input(), BotSim::echo_clicks)
        .expect("the input moved");
    assert_eq!(input.kind(), ItemKind::IronOre);
    assert!(matches!(
        sim.drive(furnace.take_input(), BotSim::echo_clicks),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn open_furnace_refuses_and_closes_another_window() {
    let (mut sim, block) = next_to(BlockKind::Furnace);
    let bot = sim.bot.clone();
    let result = sim.drive(
        bot.open_furnace(&block),
        opens(MenuKind::Generic9x3, contents(27, &[])),
    );
    assert!(matches!(result, Err(Error::InvalidArgument(_))));
    sim.tick();
    assert!(
        sim.sent()
            .iter()
            .any(|p| matches!(p, ServerboundGamePacket::ContainerClose(_))),
        "the wrong window was closed"
    );
}

fn set_slot(sim: &mut BotSim, slot: u16, item_stack: ItemStack) {
    sim.receive(
        mineflayer_engine::protocol::packets::game::ClientboundContainerSetSlot {
            container_id: WINDOW,
            state_id: 2,
            slot,
            item_stack,
        },
    );
}

fn set_level(sim: &mut BotSim, experience_level: u32) {
    sim.receive(
        mineflayer_engine::protocol::packets::game::ClientboundSetExperience {
            experience_progress: 0.,
            experience_level,
            total_experience: experience_level * 10,
        },
    );
}

/// An anvil server: a typed name puts the result in slot 2 at cost 1, and
/// taking the result costs a level.
fn anvil_server(sim: &mut BotSim, packet: &ServerboundGamePacket) {
    match packet {
        ServerboundGamePacket::RenameItem(rename) if !rename.name.is_empty() => {
            set_slot(sim, 2, ItemStack::new(ItemKind::DiamondSword, 1));
            set_data(sim, 0, 1);
        }
        ServerboundGamePacket::ContainerClick(click) if click.slot_num == 2 => {
            sim.echo_clicks(packet);
            set_level(sim, 4);
        }
        _ => sim.echo_clicks(packet),
    }
}

fn open_anvil(items: &[(usize, ItemStack)]) -> (BotSim, crate::Anvil) {
    let (mut sim, block) = next_to(BlockKind::Anvil);
    set_level(&mut sim, 5);
    let bot = sim.bot.clone();
    let anvil = sim
        .drive(
            bot.open_anvil(&block),
            opens(MenuKind::Anvil, contents(3, items)),
        )
        .expect("the anvil opened");
    (sim, anvil)
}

#[test]
fn an_anvil_renames_an_item() {
    let sword = (3 + 27, ItemStack::new(ItemKind::DiamondSword, 1));
    let (mut sim, anvil) = open_anvil(&[sword]);
    let item = anvil.slot(3 + 27).expect("the sword is in the hotbar");
    sim.drive(anvil.rename(&item, "Sting"), anvil_server)
        .expect("the rename went through");
    assert_eq!(anvil.repair_cost(), Some(1));
    assert!(
        anvil.slots().unwrap_or_default()[3..]
            .iter()
            .any(|s| s.kind() == ItemKind::DiamondSword),
        "the result is back in the inventory"
    );
    assert_eq!(sim.bot.experience().level, 4);
}

#[test]
fn an_anvil_checks_names_and_results() {
    let sword = (3 + 27, ItemStack::new(ItemKind::DiamondSword, 1));
    let (mut sim, anvil) = open_anvil(std::slice::from_ref(&sword));
    let item = anvil.slot(3 + 27).unwrap();
    let long = "x".repeat(60);
    assert!(matches!(
        sim.drive(anvil.rename(&item, &long), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));

    // a server that never makes a result: not anvil-able
    let (mut sim, anvil) =
        open_anvil(&[sword.clone(), (3 + 28, ItemStack::new(ItemKind::Dirt, 1))]);
    let one = anvil.slot(3 + 27).unwrap();
    let two = anvil.slot(3 + 28).unwrap();
    assert!(matches!(
        sim.drive(anvil.combine(&one, &two, None), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn an_enchanting_table_enchants_the_target() {
    let (mut sim, block) = next_to(BlockKind::EnchantingTable);
    let bot = sim.bot.clone();
    let items = contents(
        2,
        &[
            (2 + 27, ItemStack::new(ItemKind::DiamondSword, 1)),
            (2 + 28, ItemStack::new(ItemKind::LapisLazuli, 3)),
        ],
    );
    let table = sim
        .drive(
            bot.open_enchantment_table(&block),
            opens(MenuKind::Enchantment, items),
        )
        .expect("the table opened");
    let sword = table.slot(2 + 27).unwrap();
    let lapis = table.slot(2 + 28).unwrap();
    sim.drive(table.put_target_item(&sword), BotSim::echo_clicks)
        .expect("the sword went in");
    sim.drive(table.put_lapis(&lapis), BotSim::echo_clicks)
        .expect("the lapis went in");
    assert_eq!(
        table.target_item().map(|i| i.kind()),
        Some(ItemKind::DiamondSword)
    );
    assert!(!table.is_ready());
    assert_eq!(table.xp_seed(), -1);

    for (property, value) in [(0, 3), (1, 10), (2, 30), (3, 1234)] {
        set_data(&mut sim, property, value);
    }
    sim.tick();
    assert!(table.is_ready());
    assert_eq!(table.xp_seed(), 1234);
    assert_eq!(table.enchantments().map(|o| o.level), [3, 10, 30]);

    let enchanted = sim
        .drive(table.enchant(1), |sim, packet| {
            if let ServerboundGamePacket::ContainerButtonClick(_) = packet {
                set_slot(sim, 0, ItemStack::new(ItemKind::DiamondSword, 1));
            }
        })
        .expect("the enchant answered");
    assert_eq!(enchanted.kind(), ItemKind::DiamondSword);
    assert!(matches!(
        sim.drive(table.enchant(3), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
    let taken = sim
        .drive(table.take_target_item(), BotSim::echo_clicks)
        .expect("the target came back");
    assert_eq!(taken.kind(), ItemKind::DiamondSword);
}

#[test]
fn open_enchantment_table_needs_the_table_block() {
    let (mut sim, block) = next_to(BlockKind::Stone);
    let bot = sim.bot.clone();
    assert!(matches!(
        sim.drive(bot.open_enchantment_table(&block), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
}

fn offer(cost: (ItemKind, i32), result: (ItemKind, i32), uses: i32) -> MerchantOffer {
    MerchantOffer {
        base_cost_a: ItemCost {
            item: cost.0,
            count: cost.1,
            components: c_merchant_offers::DataComponentExactPredicate {
                expected: Vec::new(),
            },
        },
        result: ItemStack::new(result.0, result.1),
        cost_b: None,
        out_of_stock: false,
        uses,
        max_uses: 4,
        xp: 1,
        special_price_diff: 0,
        price_multiplier: 0.05,
        demand: 0,
    }
}

/// A bot next to a villager, and the villager.
fn near_villager() -> (BotSim, mineflayer_engine::EntityRef) {
    let mut sim = BotSim::new();
    sim.receive(ClientboundAddEntity {
        id: mineflayer_engine::core::entity_id::MinecraftEntityId(77),
        uuid: Default::default(),
        entity_type: EntityKind::Villager,
        position: sim.bot.position() + Vec3::new(1.5, 0., 0.),
        movement: Default::default(),
        x_rot: 0,
        y_rot: 0,
        y_head_rot: 0,
        data: 0,
    });
    sim.tick();
    let villager = sim
        .bot
        .nearest_entity(|_| true)
        .expect("the villager spawned");
    (sim, villager)
}

/// A merchant server: interacting opens the trades, selecting one stocks
/// its input and result, and clicks are confirmed.
fn merchant(
    offers: Vec<MerchantOffer>,
    items: Vec<ItemStack>,
) -> impl FnMut(&mut BotSim, &ServerboundGamePacket) {
    move |sim, packet| match packet {
        ServerboundGamePacket::Interact(_) => {
            sim.receive(ClientboundOpenScreen {
                container_id: WINDOW,
                menu_type: MenuKind::Merchant,
                title: FormattedText::from("villager"),
            });
            sim.receive(ClientboundContainerSetContent {
                container_id: WINDOW,
                state_id: 1,
                items: items.clone(),
                carried_item: ItemStack::Empty,
            });
            sim.receive(ClientboundMerchantOffers {
                container_id: WINDOW,
                offers: offers.clone(),
                villager_level: 1,
                villager_xp: 0,
                show_progress: true,
                can_restock: true,
            });
        }
        ServerboundGamePacket::SelectTrade(select) => {
            let offer = &offers[select.item as usize];
            let cost = &offer.base_cost_a;
            set_slot(sim, 0, ItemStack::new(cost.item, cost.count));
            set_slot(sim, 2, offer.result.clone());
        }
        _ => sim.echo_clicks(packet),
    }
}

#[test]
fn a_villager_lists_and_makes_trades() {
    let (mut sim, entity) = near_villager();
    let offers = vec![
        offer((ItemKind::Emerald, 2), (ItemKind::Bread, 6), 0),
        offer((ItemKind::Emerald, 1), (ItemKind::Apple, 1), 4),
    ];
    let items = contents(3, &[(3 + 27, ItemStack::new(ItemKind::Emerald, 10))]);
    let bot = sim.bot.clone();
    let villager = sim
        .drive(
            bot.open_villager(&entity),
            merchant(offers.clone(), items.clone()),
        )
        .expect("the villager opened");
    let trades = villager.trades();
    assert_eq!(trades.len(), 2);
    assert_eq!(trades[0].output_item.kind(), ItemKind::Bread);
    assert!(villager.selected_trade().is_none());

    sim.drive(villager.trade(0, Some(1)), merchant(offers.clone(), items))
        .expect("the trade went through");
    assert_eq!(
        villager.selected_trade().map(|t| t.output_item.kind()),
        Some(ItemKind::Bread)
    );
    assert_eq!(villager.trades()[0].uses, 1);

    // the second offer is used up; the first allows three more
    assert!(matches!(
        sim.drive(villager.trade(1, None), BotSim::echo_clicks),
        Err(Error::InvalidState(_))
    ));
    assert!(matches!(
        sim.drive(villager.trade(0, Some(4)), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        sim.drive(villager.trade(9, None), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn a_villager_trade_needs_the_price() {
    let (mut sim, entity) = near_villager();
    let offers = vec![offer((ItemKind::Emerald, 5), (ItemKind::Bread, 6), 0)];
    let items = contents(3, &[(3 + 27, ItemStack::new(ItemKind::Emerald, 2))]);
    let bot = sim.bot.clone();
    let villager = sim
        .drive(bot.open_villager(&entity), merchant(offers, items))
        .expect("the villager opened");
    assert!(matches!(
        sim.drive(villager.trade(0, Some(1)), BotSim::echo_clicks),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn open_villager_needs_a_villager() {
    let (mut sim, _) = near_villager();
    let bot = sim.bot.clone();
    let not_a_villager = bot.entity();
    assert!(matches!(
        sim.drive(bot.open_villager(&not_a_villager), BotSim::echo_clicks),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn a_book_is_signed_in_the_hotbar() {
    let mut sim = BotSim::new();
    sim.receive(ClientboundContainerSetContent {
        container_id: 0,
        state_id: 1,
        items: {
            let mut items = vec![ItemStack::Empty; 46];
            items[36] = ItemStack::new(ItemKind::WritableBook, 1);
            items
        },
        carried_item: ItemStack::Empty,
    });
    sim.tick();
    let bot = sim.bot.clone();
    let signing = bot.sign_book(36, vec!["page one".into()], "faye", "A Book");
    sim.drive(signing, |sim, packet| {
        if let ServerboundGamePacket::EditBook(_) = packet {
            sim.receive(
                mineflayer_engine::protocol::packets::game::ClientboundContainerSetSlot {
                    container_id: 0,
                    state_id: 2,
                    slot: 36,
                    item_stack: ItemStack::new(ItemKind::WrittenBook, 1),
                },
            );
        }
    })
    .expect("the book was signed");
    assert_eq!(
        bot.inventory().slot(36).map(|i| i.kind()),
        Some(ItemKind::WrittenBook)
    );
    // no book in the slot
    assert!(matches!(
        sim.drive(bot.write_book(37, vec!["x".into()]), BotSim::echo_clicks),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn set_command_block_needs_creative_and_a_command_block() {
    let (sim, block) = next_to(BlockKind::CommandBlock);
    let opts = crate::CommandBlockOpts::default();
    assert!(matches!(
        sim.bot.set_command_block(block.pos, "say hi", opts),
        Err(Error::InvalidState(_))
    ));
}

/// The count of `kind` in `slots`.
fn count(slots: &[ItemStack], kind: ItemKind) -> i32 {
    slots
        .iter()
        .filter(|s| s.kind() == kind)
        .map(ItemStack::count)
        .sum()
}

#[test]
fn a_chest_deposits_withdraws_and_transfers() {
    let (mut sim, block) = next_to(BlockKind::Chest);
    let items = contents(
        27,
        &[
            (0, ItemStack::new(ItemKind::Cobblestone, 40)),
            (27 + 27, ItemStack::new(ItemKind::Dirt, 20)),
            (27 + 28, ItemStack::new(ItemKind::Dirt, 5)),
        ],
    );
    let bot = sim.bot.clone();
    let chest = sim
        .drive(bot.open_chest(&block), opens(MenuKind::Generic9x3, items))
        .expect("the chest opened");
    let container = |chest: &crate::Window| chest.slots().unwrap_or_default()[..27].to_vec();
    let player = |chest: &crate::Window| chest.slots().unwrap_or_default()[27..].to_vec();

    // more than one stack's worth: two source stacks
    sim.drive(
        chest.deposit(ItemKind::Dirt, None, Some(22), None),
        BotSim::echo_clicks,
    )
    .expect("the dirt went in");
    assert_eq!(count(&container(&chest), ItemKind::Dirt), 22);
    assert_eq!(count(&player(&chest), ItemKind::Dirt), 3);

    // `None` moves one item
    sim.drive(
        chest.withdraw(ItemKind::Cobblestone, None, None, None),
        BotSim::echo_clicks,
    )
    .expect("one cobblestone came out");
    assert_eq!(count(&player(&chest), ItemKind::Cobblestone), 1);

    sim.drive(
        chest.withdraw(ItemKind::Cobblestone, None, Some(39), None),
        BotSim::echo_clicks,
    )
    .expect("the rest came out");
    assert_eq!(count(&container(&chest), ItemKind::Cobblestone), 0);
    assert_eq!(count(&player(&chest), ItemKind::Cobblestone), 40);

    // nothing left to move
    assert!(
        sim.drive(
            chest.withdraw(ItemKind::Cobblestone, None, Some(1), None),
            BotSim::echo_clicks,
        )
        .is_err()
    );
}
