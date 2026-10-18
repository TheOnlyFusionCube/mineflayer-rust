//! `bot.pathfinder.movements` — upstream's `Movements` config and its
//! mapping onto the engine's `PathfinderOpts` and move-generation constraints.

use super::*;
use mineflayer_engine::pathfinder;

/// `bot.pathfinder.movements` — upstream's `Movements` config.
///
/// Each field reaches the search: through [`PathfinderOpts`], or through
/// the port's successors fn, which reads them from the
/// engine's `CustomPathfinderState`. `dontCreateFlow` is absent: the engine
/// never breaks a block next to water or lava, which is upstream's
/// default, and has no switch for it. `allowFreeMotion` is absent: the
/// engine has no straight-line flight. Placement knobs
/// (`scafoldingBlocks`, `canPlaceOn`, `allow1by1towers`,
/// `exclusionAreas*`) are absent: the engine never places blocks while
/// pathing.
#[derive(Clone, Debug)]
pub struct Movements {
    /// `canDig` (default `true`) → `PathfinderOpts::allow_mining`.
    pub can_dig: bool,
    /// `canOpenDoors` (default `false`): the bot may walk through a closed
    /// wooden or copper door or a fence gate, and opens it when it gets
    /// there.
    pub can_open_doors: bool,
    /// `allowParkour` (default `true`) → `moves::parkour` edges are
    /// generated.
    pub allow_parkour: bool,
    /// `allowSprinting` (default `true`). With `false` the bot walks
    /// where it would sprint, and parkour jumps are limited to one-block
    /// gaps, which a walking jump crosses.
    pub allow_sprinting: bool,
    /// `dontMineUnderFallingBlock` (default `true`): never break a block
    /// under a block that falls (sand, gravel, concrete powder, …). With
    /// `false` the engine mines the falling column too.
    pub dont_mine_under_falling_block: bool,
    /// `maxDropDown` (default `4`): the highest drop of one move. The
    /// engine's moves drop at most 3 blocks, so values above 3 act as 3.
    pub max_drop_down: u32,
    /// `blocksToAvoid` (default `{fire, cobweb, lava}`) — edges entering
    /// one get +100 cost, matching upstream's unsafe-step penalty.
    pub blocks_to_avoid: HashSet<BlockKind>,
    /// `blocksCantBreak` (default `{chest}`; upstream also fills in every
    /// undiggable, which the engine already refuses to mine) — edges that
    /// would have to mine through one are dropped.
    pub blocks_cant_break: HashSet<BlockKind>,
}

impl Default for Movements {
    fn default() -> Self {
        Self {
            can_dig: true,
            can_open_doors: false,
            allow_parkour: true,
            allow_sprinting: true,
            dont_mine_under_falling_block: true,
            max_drop_down: 4,
            blocks_to_avoid: HashSet::from([BlockKind::Fire, BlockKind::Cobweb, BlockKind::Lava]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
        }
    }
}

impl Movements {
    /// The [`PathfinderOpts`] for this config — `can_dig` maps onto
    /// `allow_mining`, `allow_sprinting` onto `allow_sprint`, and every
    /// goto runs [`movements_move`] so the other fields reach the search.
    ///
    /// `retry_on_no_path: false` matches upstream: one failed search is a
    /// terminal `path_update` (`noPath`/`timeout`), not an infinite retry.
    pub(super) fn to_opts(&self) -> PathfinderOpts {
        PathfinderOpts::new()
            .successors_fn(movements_move)
            .allow_mining(self.can_dig)
            .allow_sprint(self.allow_sprinting)
            .retry_on_no_path(false)
    }
}

/// The `Movements` fields a successors fn acts on, read back through the
/// engine's `CustomPathfinderState` typemap — `SuccessorsFn` is a plain
/// `fn` that can't capture config, but `MovesCtx::custom_state` is handed
/// to it by the pathfinder itself.
#[derive(Clone)]
pub(super) struct MovementConstraints {
    allow_parkour: bool,
    allow_sprinting: bool,
    can_open_doors: bool,
    dont_mine_under_falling_block: bool,
    max_drop_down: u32,
    blocks_to_avoid: Arc<HashSet<BlockKind>>,
    blocks_cant_break: Arc<HashSet<BlockKind>>,
}

impl From<&Movements> for MovementConstraints {
    fn from(m: &Movements) -> Self {
        Self {
            allow_parkour: m.allow_parkour,
            allow_sprinting: m.allow_sprinting,
            can_open_doors: m.can_open_doors,
            dont_mine_under_falling_block: m.dont_mine_under_falling_block,
            max_drop_down: m.max_drop_down,
            blocks_to_avoid: Arc::new(m.blocks_to_avoid.clone()),
            blocks_cant_break: Arc::new(m.blocks_cant_break.clone()),
        }
    }
}

/// The successors fn every facade goto uses: the engine's `default_move`
/// set, filtered and extended by the [`Movements`] fields. With no
/// constraints installed it's exactly `default_move` (the facade always
/// installs them, so this is only a safe default for raw engine callers).
pub(super) fn movements_move(ctx: &mut moves::MovesCtx, node: RelBlockPos) {
    let Some(c) = ctx.custom_state.get::<MovementConstraints>() else {
        moves::default_move(ctx, node);
        return;
    };
    moves::basic::basic_move(ctx, node);
    if c.allow_parkour {
        let before = ctx.edges.len();
        moves::parkour::parkour_move(ctx, node);
        if !c.allow_sprinting {
            // a walking jump crosses a one-block gap, not a longer one
            let mut index = 0;
            ctx.edges.retain(|edge| {
                index += 1;
                let reach = (edge.movement.target.x - node.x).abs()
                    + (edge.movement.target.z - node.z).abs();
                index <= before || reach <= 2
            });
        }
    }
    moves::uncommon::uncommon_move(ctx, node);
    if c.can_open_doors {
        door_move(ctx, node);
    }

    let (world, mining) = (ctx.world, ctx.mining_cache);
    ctx.edges.retain_mut(|edge| {
        let target = edge.movement.target;
        if node.y - target.y > c.max_drop_down as i32 {
            return false;
        }
        // occupying `target` means breaking whatever fills the feet and
        // head blocks
        for pos in [target, target.up(1)] {
            let state = world.get_block_state(pos);
            if !moves::should_mine_block_state(state) {
                continue;
            }
            if c.blocks_cant_break.contains(&BlockKind::from(state))
                || c.dont_mine_under_falling_block
                    && mining.is_falling_block(world.get_block_state(pos.up(1)))
            {
                return false;
            }
        }
        if c.blocks_to_avoid
            .contains(&BlockKind::from(world.get_block_state(target)))
        {
            edge.cost += AVOID_STEP_COST;
        }
        true
    });
}

/// The extra cost of a move through a closed door: the bot stops to open
/// it.
const DOOR_COST: f32 = 2.;

/// Walk one block forward into a closed door or fence gate. The bridge
/// opens it when the bot reaches it (`tracking::open_door_ahead`).
fn door_move(ctx: &mut moves::MovesCtx, node: RelBlockPos) {
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let target = node + RelBlockPos::new(dx, 0, dz);
        let head = ctx.world.get_block_state(target.up(1));
        if !is_closed_door(ctx.world.get_block_state(target))
            || !(is_door(head) || pathfinder::world::is_block_state_passable(head))
            || !ctx.world.is_block_solid(target.down(1))
        {
            continue;
        }
        ctx.edges.push(pathfinder::astar::Edge {
            movement: pathfinder::astar::Movement {
                target,
                data: moves::MoveData {
                    execute: &execute_door_move,
                    is_reached: &moves::default_is_reached,
                },
            },
            cost: pathfinder::costs::WALK_ONE_BLOCK_COST + DOOR_COST,
        });
    }
}

fn execute_door_move(mut ctx: moves::ExecuteCtx) {
    let center = ctx.target.center();
    ctx.look_at(center);
    ctx.walk(mineflayer_engine::WalkDirection::Forward);
}

/// A door or fence gate that a hand opens (not an iron door).
fn is_door(state: mineflayer_engine::block::BlockState) -> bool {
    let name = BlockKind::from(state).to_str();
    name.ends_with("_fence_gate") || name.ends_with("_door") && !name.ends_with("iron_door")
}

/// [`is_door`] and closed.
pub(super) fn is_closed_door(state: mineflayer_engine::block::BlockState) -> bool {
    use mineflayer_engine::block::BlockTrait;
    is_door(state) && Box::<dyn BlockTrait>::from(state).get_property("open") == Some("false")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Movements` → `MovementConstraints` — the successors-fn-visible
    /// subset is carried over faithfully.
    #[test]
    fn movements_constraints_mapping() {
        let m = Movements {
            allow_parkour: false,
            max_drop_down: 1,
            blocks_to_avoid: HashSet::from([BlockKind::Cactus]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
            ..Default::default()
        };
        let c = MovementConstraints::from(&m);
        assert!(!c.allow_parkour);
        assert_eq!(c.max_drop_down, 1);
        assert!(c.blocks_to_avoid.contains(&BlockKind::Cactus));
        assert!(!c.blocks_to_avoid.contains(&BlockKind::Lava));
        assert!(c.blocks_cant_break.contains(&BlockKind::Chest));
    }
}

/// Each `Movements` field changes what a search through the real engine
/// world finds.
#[cfg(test)]
mod search_tests {
    use std::time::Duration;

    use mineflayer_engine::{
        BlockPos,
        block::{BlockState, BlockTrait},
        protocol::packets::game::ClientboundBlockUpdate,
        registry::builtin::BlockKind,
    };

    use super::Movements;
    use crate::{PathStatus, pathfinder::Goal, test_util::sim::BotSim};

    fn state(kind: BlockKind, properties: &[(&str, &str)]) -> BlockState {
        let mut block = Box::<dyn BlockTrait>::from(BlockState::from(kind));
        for (name, value) in properties {
            block.set_property(name, value).unwrap();
        }
        block.as_block_state()
    }

    /// A bot in a world of `blocks` and air.
    fn world(blocks: &[(BlockPos, BlockState)]) -> BotSim {
        let mut sim = BotSim::new();
        for (pos, block_state) in blocks {
            sim.receive(ClientboundBlockUpdate {
                pos: *pos,
                block_state: *block_state,
            });
        }
        sim.tick();
        sim
    }

    fn status(sim: &BotSim, movements: &Movements, start: BlockPos, goal: BlockPos) -> PathStatus {
        sim.bot
            .pathfinder()
            .get_path_from_to(
                movements,
                start,
                &Goal::Block(goal),
                Some(Duration::from_millis(500)),
            )
            .unwrap()
            .status
    }

    fn stone(pos: BlockPos) -> (BlockPos, BlockState) {
        (pos, BlockKind::Stone.into())
    }

    #[test]
    fn max_drop_down_bounds_a_drop() {
        // a ledge at y 4 and the ground three blocks lower
        let sim = world(&[stone(BlockPos::new(0, 3, 0)), stone(BlockPos::new(1, 0, 0))]);
        let (start, goal) = (BlockPos::new(0, 4, 0), BlockPos::new(1, 1, 0));
        let movements = |max_drop_down| Movements {
            max_drop_down,
            can_dig: false,
            ..Movements::default()
        };
        assert_eq!(
            status(&sim, &movements(3), start, goal),
            PathStatus::Success
        );
        assert_eq!(status(&sim, &movements(2), start, goal), PathStatus::NoPath);
    }

    /// A two-high wall between the start and the goal, with `above` on it:
    /// the only way through breaks the wall.
    fn wall_under(above: BlockState) -> BotSim {
        world(&[
            stone(BlockPos::new(0, 0, 0)),
            stone(BlockPos::new(1, 0, 0)),
            stone(BlockPos::new(2, 0, 0)),
            stone(BlockPos::new(1, 1, 0)),
            stone(BlockPos::new(1, 2, 0)),
            (BlockPos::new(1, 3, 0), above),
        ])
    }

    fn through_wall(sim: &BotSim, movements: Movements) -> PathStatus {
        status(
            sim,
            &movements,
            BlockPos::new(0, 1, 0),
            BlockPos::new(2, 1, 0),
        )
    }

    #[test]
    fn dont_mine_under_falling_block_keeps_the_sand_up() {
        let sim = wall_under(BlockKind::Sand.into());
        assert_eq!(through_wall(&sim, Movements::default()), PathStatus::NoPath);
        let movements = Movements {
            dont_mine_under_falling_block: false,
            ..Movements::default()
        };
        assert_eq!(through_wall(&sim, movements), PathStatus::Success);
    }

    #[test]
    fn without_sprinting_parkour_crosses_one_block_gaps_only() {
        let sim = world(&[stone(BlockPos::new(0, 0, 0)), stone(BlockPos::new(3, 0, 0))]);
        let (start, goal) = (BlockPos::new(0, 1, 0), BlockPos::new(3, 1, 0));
        let movements = |allow_sprinting| Movements {
            allow_sprinting,
            can_dig: false,
            ..Movements::default()
        };
        assert_eq!(
            status(&sim, &movements(true), start, goal),
            PathStatus::Success
        );
        assert_eq!(
            status(&sim, &movements(false), start, goal),
            PathStatus::NoPath
        );
    }

    #[test]
    fn can_open_doors_walks_through_a_closed_door() {
        let lower = state(BlockKind::OakDoor, &[("facing", "east"), ("half", "lower")]);
        let upper = state(BlockKind::OakDoor, &[("facing", "east"), ("half", "upper")]);
        let sim = world(&[
            stone(BlockPos::new(0, 0, 0)),
            stone(BlockPos::new(1, 0, 0)),
            stone(BlockPos::new(2, 0, 0)),
            (BlockPos::new(1, 1, 0), lower),
            (BlockPos::new(1, 2, 0), upper),
        ]);
        let movements = |can_open_doors| Movements {
            can_open_doors,
            can_dig: false,
            ..Movements::default()
        };
        let (start, goal) = (BlockPos::new(0, 1, 0), BlockPos::new(2, 1, 0));
        assert_eq!(
            status(&sim, &movements(false), start, goal),
            PathStatus::NoPath
        );
        assert_eq!(
            status(&sim, &movements(true), start, goal),
            PathStatus::Success
        );
    }
}
