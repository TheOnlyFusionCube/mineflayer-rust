use mineflayer::{
    BlockMatcher,
    engine::{
        block::BlockState,
        registry::builtin::BlockKind,
        world::{Chunk, PartialChunkStorage},
    },
    test_util::despawned_client,
};

use super::*;

#[test]
fn kind_target_is_a_state_matcher_with_upstream_defaults() {
    let target = CollectTarget::from(BlockKind::Stone);
    let BlockMatcher::States(states) = &target.opts.matching else {
        panic!("a block kind must match on palette states, not a predicate")
    };
    assert!(states.contains(&BlockState::from(BlockKind::Stone)));
    assert!(!states.contains(&BlockState::from(BlockKind::Dirt)));
    assert_eq!(target.count, 1);
    assert_eq!(target.opts.max_distance, Some(DEFAULT_MAX_DISTANCE));
    // the find window, not the requested count
    assert_eq!(target.opts.count, Some(CANDIDATE_WINDOW));
    // None = re-center on the bot every iteration
    assert!(target.opts.point.is_none());
}

#[test]
fn builder_tunes_count_distance_and_point() {
    let target = CollectTarget::new(BlockKind::Sand)
        .count(5)
        .max_distance(30.)
        .point(Vec3::new(1., 2., 3.));
    assert_eq!(target.count, 5);
    assert_eq!(target.opts.max_distance, Some(30.));
    assert_eq!(target.opts.point, Some(Vec3::new(1., 2., 3.)));
    // the find window grows to cover the asked-for count
    assert_eq!(target.opts.count, Some(5usize.max(CANDIDATE_WINDOW)));
    // `0` clamps to 1 like upstream's "collect something" contract
    assert_eq!(CollectTarget::new(BlockKind::Sand).count(0).count, 1);
    // a huge count widens the window past CANDIDATE_WINDOW
    assert_eq!(
        CollectTarget::new(BlockKind::Sand).count(200).opts.count,
        Some(200)
    );
}

#[test]
fn block_target_pins_its_position_not_its_kind() {
    let client = despawned_client();
    let block = Block::new(
        &client,
        BlockPos::new(3, 64, -5),
        BlockState::from(BlockKind::Stone),
    );
    let target = CollectTarget::from(block.clone());
    assert_eq!(target.count, 1);
    let BlockMatcher::Predicate(pred) = &target.opts.matching else {
        panic!("a pinned position must match on position, not state")
    };
    assert!(pred(&block));
    // same kind, wrong position — upstream collects the handed block
    let same_kind_elsewhere = Block::new(
        &client,
        BlockPos::new(4, 64, -5),
        BlockState::from(BlockKind::Stone),
    );
    assert!(!pred(&same_kind_elsewhere));
}

#[test]
fn block_list_collects_every_listed_position() {
    let client = despawned_client();
    let blocks = vec![
        Block::new(
            &client,
            BlockPos::new(0, 60, 0),
            BlockState::from(BlockKind::OakLog),
        ),
        Block::new(
            &client,
            BlockPos::new(8, 62, -4),
            BlockState::from(BlockKind::OakLog),
        ),
    ];
    let target = CollectTarget::from(blocks.clone());
    // upstream's array form collects all of them
    assert_eq!(target.count, 2);
    let BlockMatcher::Predicate(pred) = &target.opts.matching else {
        panic!("positions match by predicate")
    };
    assert!(blocks.iter().all(pred));
    assert_eq!(target.opts.count, Some(2usize.max(CANDIDATE_WINDOW)));
}

#[test]
fn arbitrary_predicate_target_passes_through() {
    let target = CollectTarget::new(|b: &Block| b.state.is_air());
    assert!(matches!(target.opts.matching, BlockMatcher::Predicate(_)));
    assert_eq!(target.count, 1);
}

#[test]
fn collect_opts_defaults_match_upstream() {
    let opts = CollectOpts::default();
    assert!(!opts.append);
    assert!(!opts.ignore_no_path);
    assert!(opts.chest_locations.is_none());
    assert!(opts.item_filter.is_none());
}

/// Upstream's default `itemFilter` rejects tools/weapons/armor and
/// keeps everything else.
#[test]
fn default_item_filter_keeps_loot_and_holds_gear() {
    let filter = default_item_filter();
    let item = |kind: mineflayer::engine::registry::builtin::ItemKind| Item {
        stack: mineflayer::engine::inventory::ItemStack::new(kind, 1),
        slot: 0,
    };
    use mineflayer::engine::registry::builtin::ItemKind;
    assert!(!filter(&item(ItemKind::DiamondPickaxe)));
    assert!(!filter(&item(ItemKind::DiamondSword)));
    assert!(!filter(&item(ItemKind::IronChestplate)));
    assert!(!filter(&item(ItemKind::Shield)));
    assert!(filter(&item(ItemKind::Cobblestone)));
    assert!(filter(&item(ItemKind::OakLog)));
}

#[test]
fn plugin_state_round_trip() {
    let bot = mineflayer::test_util::bot(despawned_client());
    // despawned clients can't hold state — accessors fall back to
    // defaults rather than panicking
    assert!(bot.collect_block().chest_locations().is_empty());
    assert!(bot.collect_block().movements().is_some());
    let filter = bot.collect_block().item_filter();
    let pick = Item {
        stack: mineflayer::engine::inventory::ItemStack::new(
            mineflayer::engine::registry::builtin::ItemKind::DiamondPickaxe,
            1,
        ),
        slot: 0,
    };
    assert!(!filter(&pick));
}

/// A 3×3-chunk `World` like world.rs's `test_world`.
fn vein_world() -> (mineflayer::engine::world::World, PartialChunkStorage) {
    let mut world = mineflayer::engine::world::World::default();
    let mut partial = PartialChunkStorage::default();
    for x in -1..=1 {
        for z in -1..=1 {
            partial.set(
                &mineflayer::engine::core::position::ChunkPos { x, z },
                Some(Chunk::default()),
                &mut world.chunks,
            );
        }
    }
    (world, partial)
}

#[test]
fn find_from_vein_floods_same_kind_neighbors() {
    let (world, _partial) = vein_world();
    let ore = BlockState::from(BlockKind::DiamondOre);
    // an L-shaped vein: (0,64,0) → (1,64,0) → (1,64,1), plus one
    // isolated ore the fill must not reach
    for pos in [
        BlockPos::new(0, 64, 0),
        BlockPos::new(1, 64, 0),
        BlockPos::new(1, 64, 1),
        BlockPos::new(10, 64, 10),
    ] {
        world.chunks.set_block_state(pos, ore);
    }
    world
        .chunks
        .set_block_state(BlockPos::new(2, 64, 2), BlockState::from(BlockKind::Stone));
    let found = vein_positions(
        &world,
        BlockPos::new(0, 64, 0),
        BlockKind::DiamondOre,
        100,
        16.,
        1,
    );
    assert_eq!(found.len(), 3);
    assert!(found.contains(&BlockPos::new(1, 64, 1)));
    assert!(!found.contains(&BlockPos::new(10, 64, 10)));
}

#[test]
fn find_from_vein_respects_caps() {
    let (world, _partial) = vein_world();
    let ore = BlockState::from(BlockKind::CoalOre);
    // a line of 10 ores
    for x in 0..10 {
        world.chunks.set_block_state(BlockPos::new(x, 64, 0), ore);
    }
    // maxBlocks caps the flood
    let found = vein_positions(
        &world,
        BlockPos::new(0, 64, 0),
        BlockKind::CoalOre,
        3,
        16.,
        1,
    );
    assert_eq!(found.len(), 3);
    // manhattan maxDistance bounds the reach
    let found = vein_positions(
        &world,
        BlockPos::new(0, 64, 0),
        BlockKind::CoalOre,
        100,
        3.,
        1,
    );
    assert!(found.len() <= 4);
    assert!(!found.contains(&BlockPos::new(9, 64, 0)));
}

/// `cancel_task` with nothing in flight is a plain no-op — the
/// upstream `targets.empty` early return.
#[tokio::test]
async fn cancel_task_without_a_run_is_a_noop() {
    let bot = mineflayer::test_util::bot(despawned_client());
    tokio::time::timeout(Duration::from_secs(2), bot.collect_block().cancel_task())
        .await
        .expect("idle cancel_task hung");
}

#[tokio::test]
async fn collect_before_spawn_is_not_connected() {
    let bot = mineflayer::test_util::bot(despawned_client());
    let err = bot
        .collect_block()
        .collect(BlockKind::Stone)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotConnected));
}
