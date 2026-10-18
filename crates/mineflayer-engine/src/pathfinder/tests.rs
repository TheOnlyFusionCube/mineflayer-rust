use std::{
    collections::HashSet,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use mineflayer_block::BlockState;
use mineflayer_core::position::{BlockPos, ChunkPos};
use mineflayer_registry::builtin::BlockKind;
use mineflayer_world::{Chunk, ChunkStorage, PartialChunkStorage};

use super::{
    ExecutingPath, GotoEvent, GotoId, Pathfinder, StopPathfindingEvent,
    astar::PathfinderTimeout,
    goals::BlockPosGoal,
    moves,
    simulation::{SimulatedPlayerBundle, Simulation},
};
use crate::{
    events::{Event, LocalPlayerEvents, PathEvent},
    pathfinder::goto_event::PathfinderOpts,
};

fn setup_blockposgoal_simulation(
    partial_chunks: &mut PartialChunkStorage,
    start_pos: BlockPos,
    end_pos: BlockPos,
    solid_blocks: &[BlockPos],
) -> Simulation {
    let mut simulation = setup_simulation_world(partial_chunks, start_pos, solid_blocks, &[]);

    // you can uncomment this while debugging tests to get trace logs
    // simulation.app.add_plugins(bevy_log::LogPlugin {
    //     level: bevy_log::Level::TRACE,
    //     filter: "".to_owned(),
    //     ..Default::default()
    // });

    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(end_pos)),
        opts: PathfinderOpts {
            successors_fn: moves::default_move,
            allow_mining: false,
            retry_on_no_path: true,
            allow_sprint: true,
            min_timeout: PathfinderTimeout::Nodes(1_000_000),
            max_timeout: PathfinderTimeout::Nodes(5_000_000),
            total_timeout: None,
        },
        id: GotoId(7),
    });
    simulation
}

fn setup_simulation_world(
    partial_chunks: &mut PartialChunkStorage,
    start_pos: BlockPos,
    solid_blocks: &[BlockPos],
    extra_blocks: &[(BlockPos, BlockState)],
) -> Simulation {
    let mut chunk_positions = HashSet::new();
    for block_pos in solid_blocks {
        chunk_positions.insert(ChunkPos::from(block_pos));
    }
    for (block_pos, _) in extra_blocks {
        chunk_positions.insert(ChunkPos::from(block_pos));
    }

    let mut chunks = ChunkStorage::default();
    for chunk_pos in chunk_positions {
        partial_chunks.set(&chunk_pos, Some(Chunk::default()), &mut chunks);
    }
    for block_pos in solid_blocks {
        chunks.set_block_state(*block_pos, BlockKind::Stone.into());
    }
    for (block_pos, block_state) in extra_blocks {
        chunks.set_block_state(*block_pos, *block_state);
    }

    let player = SimulatedPlayerBundle::new(start_pos.center_bottom());
    Simulation::new(chunks, player)
}

pub fn assert_simulation_reaches(simulation: &mut Simulation, ticks: usize, end_pos: BlockPos) {
    wait_until_bot_starts_moving(simulation);
    for _ in 0..ticks {
        simulation.tick();
    }
    assert_eq!(BlockPos::from(simulation.position()), end_pos);
}

pub fn wait_until_bot_starts_moving(simulation: &mut Simulation) {
    let start_pos = simulation.position();
    let start_time = Instant::now();
    while simulation.position() == start_pos
        && !simulation.is_mining()
        && start_time.elapsed() < Duration::from_millis(5000)
    {
        simulation.tick();
        thread::yield_now();
    }
}

#[test]
fn test_simple_forward() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(0, 71, 1),
        &[BlockPos::new(0, 70, 0), BlockPos::new(0, 70, 1)],
    );
    assert_simulation_reaches(&mut simulation, 20, BlockPos::new(0, 71, 1));
}

#[test]
fn test_double_diagonal_with_walls() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(2, 71, 2),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(1, 70, 1),
            BlockPos::new(2, 70, 2),
            BlockPos::new(1, 72, 0),
            BlockPos::new(2, 72, 1),
        ],
    );
    assert_simulation_reaches(&mut simulation, 30, BlockPos::new(2, 71, 2));
}

#[test]
fn test_jump_with_sideways_momentum() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 3),
        BlockPos::new(5, 76, 0),
        &[
            BlockPos::new(0, 70, 3),
            BlockPos::new(0, 70, 2),
            BlockPos::new(0, 70, 1),
            BlockPos::new(0, 70, 0),
            BlockPos::new(1, 71, 0),
            BlockPos::new(2, 72, 0),
            BlockPos::new(3, 73, 0),
            BlockPos::new(4, 74, 0),
            BlockPos::new(5, 75, 0),
        ],
    );
    assert_simulation_reaches(&mut simulation, 120, BlockPos::new(5, 76, 0));
}

#[test]
fn test_parkour_2_block_gap() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(0, 71, 3),
        &[BlockPos::new(0, 70, 0), BlockPos::new(0, 70, 3)],
    );
    assert_simulation_reaches(&mut simulation, 40, BlockPos::new(0, 71, 3));
}

#[test]
fn test_descend_and_parkour_2_block_gap() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(3, 67, 4),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 69, 1),
            BlockPos::new(0, 68, 2),
            BlockPos::new(0, 67, 3),
            BlockPos::new(0, 66, 4),
            BlockPos::new(3, 66, 4),
        ],
    );
    assert_simulation_reaches(&mut simulation, 100, BlockPos::new(3, 67, 4));
}

#[test]
fn test_small_descend_and_parkour_2_block_gap() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(0, 70, 5),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 70, 1),
            BlockPos::new(0, 69, 2),
            BlockPos::new(0, 69, 5),
        ],
    );
    assert_simulation_reaches(&mut simulation, 40, BlockPos::new(0, 70, 5));
}

#[test]
fn test_quickly_descend() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(0, 68, 3),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 69, 1),
            BlockPos::new(0, 68, 2),
            BlockPos::new(0, 67, 3),
        ],
    );
    assert_simulation_reaches(&mut simulation, 60, BlockPos::new(0, 68, 3));
}

#[test]
fn test_2_gap_ascend_thrice() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(3, 74, 0),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 71, 3),
            BlockPos::new(3, 72, 3),
            BlockPos::new(3, 73, 0),
        ],
    );
    assert_simulation_reaches(&mut simulation, 60, BlockPos::new(3, 74, 0));
}

#[test]
fn test_consecutive_3_gap_parkour() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(4, 71, 12),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 70, 4),
            BlockPos::new(0, 70, 8),
            BlockPos::new(0, 70, 12),
            BlockPos::new(4, 70, 12),
        ],
    );
    assert_simulation_reaches(&mut simulation, 80, BlockPos::new(4, 71, 12));
}

#[test]
fn test_jumps_with_more_sideways_momentum() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_blockposgoal_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        BlockPos::new(4, 74, 9),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(0, 70, 1),
            BlockPos::new(0, 70, 2),
            BlockPos::new(0, 71, 3),
            BlockPos::new(0, 72, 6),
            BlockPos::new(0, 73, 9),
            // this is the point where the bot might fall if it has too much momentum
            BlockPos::new(2, 73, 9),
            BlockPos::new(4, 73, 9),
        ],
    );
    assert_simulation_reaches(&mut simulation, 80, BlockPos::new(4, 74, 9));
}

#[test]
fn test_mine_through_non_colliding_block() {
    let mut partial_chunks = PartialChunkStorage::default();

    let mut simulation = setup_simulation_world(
        &mut partial_chunks,
        BlockPos::new(0, 72, 1),
        &[BlockPos::new(0, 71, 1)],
        &[
            (BlockPos::new(0, 71, 0), BlockKind::SculkVein.into()),
            (BlockPos::new(0, 70, 0), BlockKind::GrassBlock.into()),
            // this is an extra check to make sure that we don't accidentally break the block
            // below (since tnt will break instantly)
            (BlockPos::new(0, 69, 0), BlockKind::Tnt.into()),
        ],
    );

    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(0, 70, 0))),
        opts: PathfinderOpts::new()
            .min_timeout(PathfinderTimeout::Nodes(1_000_000))
            .max_timeout(PathfinderTimeout::Nodes(5_000_000)),
        id: GotoId(7),
    });

    assert_simulation_reaches(&mut simulation, 200, BlockPos::new(0, 70, 0));
}

/// A2: a stop must clear `goal` and `is_calculating` even when the
/// entity has no `ExecutingPath` (a calculation in flight, or none was
/// ever started). The port's `stop_pathfinding_patched` existed only to
/// repair these two fields from outside the engine.
#[test]
fn stop_clears_goal_and_calculating_without_an_executing_path() {
    let mut partial_chunks = PartialChunkStorage::default();
    let mut simulation = setup_simulation_world(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        &[BlockPos::new(0, 70, 0)],
        &[],
    );
    // PreUpdate's `add_default_pathfinder` inserts the component.
    simulation.tick();

    {
        let world = simulation.app.world_mut();
        let mut pathfinder = world.get_mut::<Pathfinder>(simulation.entity).unwrap();
        pathfinder.goal = Some(Arc::new(BlockPosGoal(BlockPos::new(0, 70, 0))));
        pathfinder.is_calculating = true;
    }
    assert!(
        simulation.get_component::<ExecutingPath>().is_none(),
        "the test needs the no-ExecutingPath case"
    );

    simulation
        .app
        .world_mut()
        .write_message(StopPathfindingEvent {
            entity: simulation.entity,
            force: false,
        });
    simulation.tick();

    let pathfinder = simulation.component::<Pathfinder>();
    assert!(pathfinder.goal.is_none(), "goal must be cleared");
    assert!(!pathfinder.is_calculating, "is_calculating must be cleared");
}

fn pathfinder_event_simulation(
    partial_chunks: &mut PartialChunkStorage,
    start: BlockPos,
    solid: &[BlockPos],
) -> (Simulation, tokio::sync::mpsc::UnboundedReceiver<Event>) {
    let mut simulation = setup_simulation_world(partial_chunks, start, solid, &[]);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    simulation
        .app
        .world_mut()
        .entity_mut(simulation.entity)
        .insert(LocalPlayerEvents(tx));
    (simulation, rx)
}

/// A1: a search with no route reports `PathFound` on the client event
/// stream (empty path, not partial, with the real search time). Before
/// the engine emitted these events, nothing ever arrived and the port had
/// to infer the outcome by diffing components each tick.
#[test]
fn pathfinder_reports_no_path_as_an_event() {
    let mut partial_chunks = PartialChunkStorage::default();
    // The player is sealed inside a one-block cell; the goal sits outside
    // it. The search space is tiny, so A* exhausts it immediately.
    let (mut simulation, mut rx) = pathfinder_event_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        &[
            BlockPos::new(0, 70, 0),
            BlockPos::new(1, 71, 0),
            BlockPos::new(-1, 71, 0),
            BlockPos::new(0, 71, 1),
            BlockPos::new(0, 71, -1),
            BlockPos::new(0, 72, 0),
        ],
    );

    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(5, 71, 0))),
        opts: PathfinderOpts {
            successors_fn: moves::default_move,
            allow_mining: false,
            retry_on_no_path: true,
            allow_sprint: true,
            min_timeout: PathfinderTimeout::Nodes(1_000_000),
            max_timeout: PathfinderTimeout::Nodes(5_000_000),
            total_timeout: None,
        },
        id: GotoId(7),
    });

    let mut found = None;
    'ticks: for _ in 0..2_000 {
        simulation.tick();
        loop {
            match rx.try_recv() {
                Ok(Event::Pathfinder(PathEvent::PathFound {
                    goto,
                    path,
                    is_partial,
                    elapsed,
                })) => {
                    assert_eq!(goto, GotoId(7), "the search reports its goto");
                    found = Some((path, is_partial, elapsed));
                    break 'ticks;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // The search runs on the async compute task pool; give it time.
        thread::sleep(Duration::from_millis(1));
    }
    let (path, is_partial, elapsed) = found.expect("no path event arrived on the event channel");
    assert!(
        path.is_empty(),
        "no route must report an empty path: {path:?}"
    );
    // `is_partial == false` is reserved for paths that reach the goal;
    // an exhausted or budget-cut search returns an incomplete result.
    assert!(is_partial, "a no-route search is incomplete");
    assert!(elapsed > Duration::ZERO, "the search time must be real");
}

/// A1: the already-at-goal fast path reports `GoalReached`.
#[test]
fn pathfinder_reports_reached_goal_as_an_event() {
    let mut partial_chunks = PartialChunkStorage::default();
    let (mut simulation, mut rx) = pathfinder_event_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        &[BlockPos::new(0, 70, 0)],
    );

    // Goal == the player's own block: `goto_listener` takes the
    // already-there path and never starts a search.
    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(0, 71, 0))),
        opts: PathfinderOpts::new()
            .min_timeout(PathfinderTimeout::Nodes(1_000_000))
            .max_timeout(PathfinderTimeout::Nodes(5_000_000)),
        id: GotoId(7),
    });

    let mut reached = false;
    'ticks: for _ in 0..50 {
        simulation.tick();
        loop {
            match rx.try_recv() {
                Ok(Event::Pathfinder(PathEvent::GoalReached { goto })) => {
                    assert_eq!(goto, GotoId(7));
                    reached = true;
                    break 'ticks;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    assert!(reached, "GoalReached never arrived on the event channel");
}

/// A1: a stop reports `Stopped` on the client event stream.
#[test]
fn pathfinder_reports_stop_as_an_event() {
    let mut partial_chunks = PartialChunkStorage::default();
    let (mut simulation, mut rx) = pathfinder_event_simulation(
        &mut partial_chunks,
        BlockPos::new(0, 71, 0),
        &[BlockPos::new(0, 70, 0)],
    );

    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(0, 71, 5))),
        opts: PathfinderOpts::new()
            .min_timeout(PathfinderTimeout::Nodes(1_000_000))
            .max_timeout(PathfinderTimeout::Nodes(5_000_000)),
        id: GotoId(7),
    });
    // Let the goto land (goal set, search running or done).
    simulation.tick();

    simulation
        .app
        .world_mut()
        .write_message(StopPathfindingEvent {
            entity: simulation.entity,
            force: false,
        });

    let mut stopped = false;
    'ticks: for _ in 0..50 {
        simulation.tick();
        loop {
            match rx.try_recv() {
                Ok(Event::Pathfinder(PathEvent::Stopped { goto })) => {
                    assert_eq!(goto, GotoId(7));
                    stopped = true;
                    break 'ticks;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    assert!(stopped, "Stopped never arrived on the event channel");
}

/// A goto whose partial searches use its total time gets one `Timeout`
/// and keeps its goal, so the bot keeps walking toward it (upstream's
/// `thinkTimeout` leaves `stateGoal` installed).
#[test]
fn a_goto_over_its_total_time_times_out_once_and_keeps_its_goal() {
    let mut partial_chunks = PartialChunkStorage::default();
    // a floor toward a goal past its end: the first search ends partial
    // with a path along the floor, the repaths at its end find nothing
    let floor: Vec<BlockPos> = (0..4).map(|x| BlockPos::new(x, 70, 0)).collect();
    let (mut simulation, mut rx) =
        pathfinder_event_simulation(&mut partial_chunks, BlockPos::new(0, 71, 0), &floor);
    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(30, 71, 0))),
        opts: PathfinderOpts::new()
            .allow_mining(false)
            .min_timeout(PathfinderTimeout::Nodes(500))
            .max_timeout(PathfinderTimeout::Nodes(500))
            .total_timeout(Duration::ZERO),
        id: GotoId(9),
    });

    let (mut found, mut partial, mut timeouts) = (0, 0, 0);
    for _ in 0..1000 {
        simulation.tick();
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Pathfinder(PathEvent::PathFound {
                    goto,
                    is_partial,
                    path,
                    ..
                }) => {
                    assert_eq!(goto, GotoId(9));
                    found += 1;
                    if is_partial && !path.is_empty() {
                        partial += 1;
                    }
                }
                Event::Pathfinder(PathEvent::Timeout { goto, path }) => {
                    assert_eq!(goto, GotoId(9));
                    assert!(!path.is_empty(), "the timeout carries the partial path");
                    timeouts += 1;
                }
                _ => {}
            }
        }
        if found >= 5 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        partial >= 1 && found >= 5,
        "{partial} partial of {found} searches"
    );
    assert_eq!(timeouts, 1, "the timeout goes out once");
    assert!(
        simulation.component::<Pathfinder>().goal.is_some(),
        "the goal stays"
    );
}

#[test]
fn a_partial_search_returns_without_a_pause() {
    use bevy_ecs::entity::Entity;
    use parking_lot::RwLock;

    use super::{CalculatePathCtx, calculate_path, custom_state::CustomPathfinderState};
    use crate::pathfinder::mining::MiningCache;

    let mut partial_chunks = PartialChunkStorage::default();
    let mut chunks = ChunkStorage::default();
    let chunk_pos = ChunkPos::new(0, 0);
    partial_chunks.set(&chunk_pos, Some(Chunk::default()), &mut chunks);
    let start = BlockPos::new(0, 71, 0);
    chunks.set_block_state(start.down(1), BlockKind::Stone.into());

    let started = Instant::now();
    let found = calculate_path(CalculatePathCtx {
        entity: Entity::PLACEHOLDER,
        start,
        goal: Arc::new(BlockPosGoal(BlockPos::new(0, 71, 1_000))),
        world_lock: Arc::new(RwLock::new(chunks.into())),
        goto_id_atomic: Default::default(),
        goto: GotoId::default(),
        mining_cache: MiningCache::new(None),
        custom_state: CustomPathfinderState::default(),
        opts: PathfinderOpts::new()
            .allow_mining(false)
            .min_timeout(PathfinderTimeout::Nodes(100))
            .max_timeout(PathfinderTimeout::Nodes(100)),
    })
    .expect("nothing superseded the search");

    assert!(found.is_partial, "the goal is out of reach");
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "took {:?}",
        started.elapsed()
    );
}

/// How many sprint starts the executor sends on a straight 8-block walk.
fn sprint_starts(allow_sprint: bool) -> usize {
    use bevy_ecs::message::{MessageCursor, Messages};
    use mineflayer_client::StartSprintEvent;

    let mut partial_chunks = PartialChunkStorage::default();
    let floor: Vec<BlockPos> = (0..10).map(|z| BlockPos::new(0, 70, z)).collect();
    let mut simulation =
        setup_simulation_world(&mut partial_chunks, BlockPos::new(0, 71, 0), &floor, &[]);
    simulation.app.world_mut().write_message(GotoEvent {
        entity: simulation.entity,
        goal: Arc::new(BlockPosGoal(BlockPos::new(0, 71, 8))),
        opts: PathfinderOpts::new()
            .allow_mining(false)
            .allow_sprint(allow_sprint)
            .min_timeout(PathfinderTimeout::Nodes(100_000))
            .max_timeout(PathfinderTimeout::Nodes(100_000)),
        id: GotoId(11),
    });
    let mut cursor = MessageCursor::<StartSprintEvent>::default();
    let mut starts = 0;
    wait_until_bot_starts_moving(&mut simulation);
    for _ in 0..60 {
        simulation.tick();
        let messages = simulation.app.world().resource::<Messages<StartSprintEvent>>();
        starts += cursor.read(messages).count();
    }
    assert_eq!(BlockPos::from(simulation.position()), BlockPos::new(0, 71, 8));
    starts
}

#[test]
fn a_goto_without_sprint_walks() {
    assert!(sprint_starts(true) > 0, "the default sprints");
    assert_eq!(sprint_starts(false), 0);
}
