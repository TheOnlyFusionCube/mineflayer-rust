
use mineflayer_engine::{
    block::BlockState,
    registry::builtin::BlockKind,
    world::{Chunk, PartialChunkStorage},
};

use super::*;

pub(super) fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}

/// prismarine's `diggable` flag: the air kinds are undiggable despite
/// hardness 0; bedrock-like blocks are undiggable; fluids stay
/// diggable like upstream's data.
#[test]
fn diggable_matches_the_prismarine_flag() {
    let client = crate::test_util::despawned_client();
    let block = |kind| Block::new(&client, BlockPos::new(0, 64, 0), BlockState::from(kind));
    for kind in [BlockKind::Air, BlockKind::VoidAir, BlockKind::CaveAir] {
        assert!(!block(kind).diggable(), "{kind:?} must not be diggable");
    }
    for kind in [BlockKind::Stone, BlockKind::Water, BlockKind::ShortGrass] {
        assert!(block(kind).diggable(), "{kind:?} must be diggable");
    }
}

/// `boundingBox` is typed but keeps upstream's strings; `short_name`
/// is upstream's un-namespaced `block.name`.
#[test]
fn bounding_box_and_short_name_match_upstream() {
    let client = crate::test_util::despawned_client();
    let block = |kind| Block::new(&client, BlockPos::new(0, 64, 0), BlockState::from(kind));
    assert_eq!(block(BlockKind::Stone).bounding_box(), BoundingBox::Block);
    assert_eq!(block(BlockKind::Air).bounding_box(), BoundingBox::Empty);
    assert_eq!(BoundingBox::Block.as_str(), "block");
    assert_eq!(BoundingBox::Empty.as_str(), "empty");
    assert_eq!(block(BlockKind::GrassBlock).name(), "minecraft:grass_block");
    assert_eq!(block(BlockKind::GrassBlock).short_name(), "grass_block");
}

/// A 3×3-chunk `World` for tests. Returns the `PartialChunkStorage`
/// too — `ChunkStorage` only holds `Weak` chunk references, so the
/// caller must keep the partial alive or `get` returns `None`.
pub(super) fn test_world() -> (World, PartialChunkStorage) {
    let mut world = World::default();
    let mut partial = PartialChunkStorage::default();
    for x in -1..=1 {
        for z in -1..=1 {
            partial.set(
                &ChunkPos { x, z },
                Some(Chunk::default()),
                &mut world.chunks,
            );
        }
    }
    (world, partial)
}

/// A `WorldRef` over a fresh 3×3 test world with `blocks` placed,
/// plus the shared world (for mutations between assertions) and the
/// partial storage the caller must keep alive.
pub(super) fn world_view(
    blocks: &[(BlockPos, BlockState)],
) -> (
    WorldRef,
    std::sync::Arc<parking_lot::RwLock<World>>,
    PartialChunkStorage,
) {
    let client = crate::test_util::live_entity_client(());
    let (world, partial) = test_world();
    for (pos, state) in blocks {
        world.chunks.set_block_state(*pos, *state);
    }
    let shared = std::sync::Arc::new(parking_lot::RwLock::new(world));
    client
        .ecs
        .write()
        .entity_mut(client.entity)
        .insert(WorldHolder::new(client.entity, shared.clone()));
    (WorldRef::new(client), shared, partial)
}

#[test]
fn find_blocks_closest_first_and_count() {
    let (world, _partial) = test_world();
    world
        .chunks
        .set_block_state(BlockPos { x: 8, y: 0, z: 0 }, stone());
    world
        .chunks
        .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
    world
        .chunks
        .set_block_state(BlockPos { x: 0, y: 5, z: 0 }, stone());
    let states = BlockStates::from([BlockKind::Stone]);

    let found = find_states(&world, BlockPos::default(), &states, 16., 10);
    assert_eq!(
        found,
        vec![
            BlockPos { x: 0, y: 0, z: 3 },
            BlockPos { x: 0, y: 5, z: 0 },
            BlockPos { x: 8, y: 0, z: 0 },
        ]
    );
    let found = find_states(&world, BlockPos::default(), &states, 16., 2);
    assert_eq!(found.len(), 2);
}

#[test]
fn find_blocks_respects_max_distance() {
    let (world, _partial) = test_world();
    world
        .chunks
        .set_block_state(BlockPos { x: 10, y: 0, z: 0 }, stone());
    let states = BlockStates::from([BlockKind::Stone]);
    assert!(find_states(&world, BlockPos::default(), &states, 4., 1).is_empty());
    assert_eq!(
        find_states(&world, BlockPos::default(), &states, 16., 1),
        vec![BlockPos { x: 10, y: 0, z: 0 }]
    );
}

#[test]
fn find_blocks_predicate() {
    let (world_ref, _shared, _partial) = world_view(&[
        (BlockPos { x: 2, y: 0, z: 0 }, stone()),
        (BlockPos { x: -2, y: 0, z: 0 }, stone()),
    ]);
    // the predicate reads the world again — it runs with no world
    // lock held, so the re-read is safe
    let reread = WorldRef::new(world_ref.client().clone());
    let found = world_ref.find_blocks(
        &FindBlocksOpts::new(move |b: &Block| {
            reread.block_at(b.pos).is_some() && b.pos.x > 0 && b.state == stone()
        })
        .max_distance(16.)
        .count(10),
    );
    assert_eq!(found, vec![BlockPos { x: 2, y: 0, z: 0 }]);
}

/// A predicate that reads the world again must not deadlock, also when
/// a writer waits on the world lock. With the lock held across the
/// predicate, the queued writer blocks and the predicate's own re-read
/// then blocks behind it (a `parking_lot` read lock is not re-entrant
/// when a writer waits) — the scan never finishes.
#[test]
fn predicate_may_read_the_world_with_a_writer_queued() {
    let (world_ref, shared, _partial) = world_view(&[(BlockPos { x: 2, y: 0, z: 0 }, stone())]);
    let client = world_ref.client().clone();

    // the predicate tells the test when it first runs; the writer is
    // queued at that moment
    let (pred_tx, pred_rx) = std::sync::mpsc::channel::<()>();
    let probe = WorldRef::new(client.clone());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reread = WorldRef::new(client);
        let first = std::sync::atomic::AtomicBool::new(true);
        let found = probe.find_blocks(
            &FindBlocksOpts::new(move |b: &Block| {
                if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let _ = pred_tx.send(());
                    // let the test queue the writer while this predicate
                    // is still running
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                reread.block_at(b.pos).is_some() && b.state == stone()
            })
            .max_distance(16.)
            .count(10),
        );
        let _ = done_tx.send(found);
    });
    pred_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("predicate never ran");
    let writer = std::thread::spawn(move || {
        let guard = shared.write();
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(guard);
    });
    let found = done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect(
            "find_blocks deadlocked: the predicate's world read blocked behind the queued writer",
        );
    assert_eq!(found, vec![BlockPos { x: 2, y: 0, z: 0 }]);
    writer.join().unwrap();
}

#[test]
fn raycast_hits_and_misses() {
    let (world, _partial) = test_world();
    world
        .chunks
        .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
    let from = Vec3 {
        x: 0.5,
        y: 0.5,
        z: 0.5,
    };
    assert_eq!(
        raycast_blocks(
            &world,
            from,
            Vec3 {
                x: 0.,
                y: 0.,
                z: 1.
            },
            10.,
        )
        .map(|(pos, _)| pos),
        Some(BlockPos { x: 0, y: 0, z: 3 })
    );
    // nothing but air along +x.
    assert!(
        raycast_blocks(
            &world,
            from,
            Vec3 {
                x: 1.,
                y: 0.,
                z: 0.
            },
            10.,
        )
        .is_none()
    );
}

pub(super) fn short_grass() -> BlockState {
    BlockState::from(BlockKind::ShortGrass)
}

#[test]
fn raycast_no_matcher_clips_collision_shapes() {
    // Short grass has an outline but *no* collision shape — the
    // no-matcher ray (upstream `iter.intersect(block.shapes)`)
    // passes through it to the stone behind.
    let (world, _partial) = test_world();
    world
        .chunks
        .set_block_state(BlockPos { x: 0, y: 0, z: 2 }, short_grass());
    world
        .chunks
        .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
    let from = Vec3 {
        x: 0.5,
        y: 0.5,
        z: 0.5,
    };
    let dir = Vec3 {
        x: 0.,
        y: 0.,
        z: 1.,
    };
    assert_eq!(
        raycast_blocks(&world, from, dir, 10.).map(|(pos, _)| pos),
        Some(BlockPos { x: 0, y: 0, z: 3 })
    );
}

#[test]
fn raycast_matcher_accepts_voxel_entry() {
    // Upstream `WorldSync.raycast`: with a matcher the first voxel
    // it accepts wins — no shape clip — so matchers reach
    // empty/partial shapes and intervening blocks never clip. The
    // matcher runs outside the world lock (`raycast_unlocked`).
    let (world_ref, _shared, _partial) = world_view(&[
        (BlockPos { x: 0, y: 0, z: 2 }, stone()),
        (BlockPos { x: 0, y: 0, z: 3 }, short_grass()),
    ]);
    let target = BlockPos { x: 0, y: 0, z: 3 };
    let from = Vec3 {
        x: 0.5,
        y: 0.5,
        z: 0.5,
    };
    let dir = Vec3 {
        x: 0.,
        y: 0.,
        z: 1.,
    };
    // no matcher: the stone's collision shape wins.
    assert_eq!(
        world_ref.raycast(from, dir, 10., None).map(|b| b.pos),
        Some(BlockPos { x: 0, y: 0, z: 2 })
    );
    // a matcher sails past the stone and accepts the grass on entry.
    let accept = BlockMatcher::Predicate(Box::new(move |b: &Block| b.pos == target));
    assert_eq!(
        world_ref
            .raycast(from, dir, 10., Some(&accept))
            .map(|b| b.pos),
        Some(target)
    );
    // a rejecting matcher must pass every block by.
    let reject = BlockMatcher::Predicate(Box::new(|_: &Block| false));
    assert!(world_ref.raycast(from, dir, 10., Some(&reject)).is_none());
}

#[test]
fn can_see_block_partial_outline() {
    // The eye→voxel-centre ray never clips an empty-collision block
    // (short grass), yet `can_see_accepts` accepts it on voxel
    // entry — the case the outline-clip version false-negatived on.
    let (world_ref, shared, _partial) =
        world_view(&[(BlockPos { x: 0, y: 0, z: 3 }, short_grass())]);
    let target = BlockPos { x: 0, y: 0, z: 3 };
    let from = Vec3 {
        x: 0.5,
        y: 2.5,
        z: 0.5,
    };
    let to = Vec3 {
        x: 0.5,
        y: 0.5,
        z: 3.5,
    };
    let range = from.distance_to(to);
    let dir = (to - from) * (1. / range);
    let accept = BlockMatcher::Predicate(Box::new(move |b: &Block| {
        can_see_accepts(target, from, to, b.pos, b.state)
    }));
    // nothing clips (grass has no collision shape), but the target's
    // voxel is entered → accepted.
    assert!(world_ref.raycast(from, dir, range, None).is_none());
    assert_eq!(
        world_ref
            .raycast(from, dir, range, Some(&accept))
            .map(|b| b.pos),
        Some(target)
    );
    // an intervening block whose collision clips is accepted first
    // → the target is *not* seen through it.
    shared
        .write()
        .chunks
        .set_block_state(BlockPos { x: 0, y: 1, z: 2 }, stone());
    assert_eq!(
        world_ref
            .raycast(from, dir, range, Some(&accept))
            .map(|b| b.pos),
        Some(BlockPos { x: 0, y: 1, z: 2 })
    );
}

#[test]
fn sign_lines_validates() {
    assert!(sign_lines("a\nb").is_ok());
    assert_eq!(sign_lines("a\nb").unwrap()[2], "");
    assert!(sign_lines("a\nb\nc\nd").is_ok());
    assert!(sign_lines("a\nb\nc\nd\ne").is_err());
    assert!(sign_lines(&"x".repeat(46)).is_err());
}

/// `findBlocks`' `||` coercion: `maxDistance: 0`/`count: 0` are falsy
/// upstream and take the default just like an unset option.
#[test]
fn find_blocks_falsy_defaults() {
    let client = crate::test_util::live_entity_client(());
    let (world, _partial) = test_world();
    world
        .chunks
        .set_block_state(BlockPos { x: 10, y: 0, z: 0 }, stone());
    let shared = std::sync::Arc::new(parking_lot::RwLock::new(world));
    client
        .ecs
        .write()
        .entity_mut(client.entity)
        .insert(WorldHolder::new(client.entity, shared));
    let world = WorldRef::new(client);

    // `max_distance: 0` upstream falls back to 16 — far enough to
    // see the stone 10 blocks out; `count: 0` falls back to 1.
    let mut opts = FindBlocksOpts::new(BlockKind::Stone);
    opts.max_distance = Some(0.);
    opts.count = Some(0);
    assert_eq!(
        world.find_blocks(&opts),
        vec![BlockPos { x: 10, y: 0, z: 0 }]
    );
    // NaN is falsy too; a nonzero override still wins.
    opts.max_distance = Some(f64::NAN);
    opts.count = Some(5);
    assert_eq!(world.find_blocks(&opts).len(), 1);
    opts.max_distance = Some(4.);
    assert!(world.find_blocks(&opts).is_empty());
}

#[test]
fn location_fields_match_upstream_formulas() {
    let loc = Location::new(Vec3 {
        x: 20.5,
        y: 70.0,
        z: -18.2,
    });
    assert_eq!(loc.floored, BlockPos::new(20, 70, -19));
    // upstream `modulus` floors the remainder: -19 -> 13, not -3
    assert_eq!(loc.block_point, BlockPos::new(4, 6, 13));
    assert_eq!(loc.chunk_corner, BlockPos::new(16, 64, -32));
    assert_eq!(loc.block_index, 4 + 16 * 13 + 256 * 6);
    assert_eq!(loc.biome_block_index, 4 + 16 * 13);
    // chunkYIndex floors the *unfloored* absolute y / 16
    assert_eq!(loc.chunk_y_index, 4);
}

/// Before `c_set_default_spawn_position` the spawn point is unknown,
/// not the origin.
#[test]
fn spawn_point_is_none_before_the_packet() {
    let bot = crate::test_util::bot(crate::test_util::despawned_client());
    assert_eq!(bot.spawn_point(), None);
}

#[test]
fn location_negative_y_wraps_like_upstream() {
    let loc = Location::new(Vec3 {
        x: 0.,
        y: -0.5,
        z: 0.,
    });
    assert_eq!(loc.floored, BlockPos::new(0, -1, 0));
    assert_eq!(loc.block_point, BlockPos::new(0, 15, 0));
    assert_eq!(loc.chunk_corner, BlockPos::new(0, -16, 0));
    assert_eq!(loc.block_index, 256 * 15);
    assert_eq!(loc.chunk_y_index, -1);
}
