//! `bot.pathfinder.movements` — upstream's `Movements` config and its
//! mapping onto the engine's `PathfinderOpts` and move-generation constraints.

use super::*;

/// `bot.pathfinder.movements` — upstream's `Movements` config.
///
/// Fields the engine (0.16) can honor are applied through [`PathfinderOpts`]
/// plus `CustomPathfinderState`; the rest are accepted for drop-in parity
/// and documented as unhonored (`diverges` rows in api-parity.md).
/// Placement knobs (`scafoldingBlocks`, `canPlaceOn`, `allow1by1towers`,
/// `exclusionAreas*`) are absent entirely: the engine never places blocks
/// while pathing, so they'd be dead config.
#[derive(Clone, Debug)]
pub struct Movements {
    /// `canDig` (default `true`) → `PathfinderOpts::allow_mining`.
    pub can_dig: bool,
    /// `canOpenDoors` (default `false`). Unhonored: the engine never opens
    /// doors or gates — closed ones are simply impassable.
    pub can_open_doors: bool,
    /// `allowParkour` (default `true`) → `moves::parkour` edges are
    /// generated.
    pub allow_parkour: bool,
    /// `allowSprinting` (default `true`). Unhonored: the engine picks
    /// walk/sprint inside each edge's executor; there is no global
    /// toggle.
    pub allow_sprinting: bool,
    /// `allowFreeMotion` (default `false`). Unhonored: upstream's
    /// fly-straight-to-goal shortcut has no engine analog.
    pub allow_free_motion: bool,
    /// `dontCreateFlow` (default `true`). Unhonored: the engine doesn't model
    /// liquid flow released by mining.
    pub dont_create_flow: bool,
    /// `dontMineUnderFallingBlock` (default `true`). Unhonored: the engine's
    /// break costs don't special-case gravity blocks.
    pub dont_mine_under_falling_block: bool,
    /// `maxDropDown` (default `4`). Unhonored: the engine bounds falls by
    /// cost constants, not an exposed cap.
    pub max_drop_down: u32,
    /// `blocksToAvoid` (default `{fire, cobweb, lava}`) — edges entering
    /// one get +100 cost, matching upstream's unsafe-step penalty.
    pub blocks_to_avoid: HashSet<BlockKind>,
    /// `blocksCantBreak` (default `{chest}`; upstream also fills in every
    /// undiggable, which the engine already refuses to mine) — edges that
    /// would have to mine through one are dropped. Approximation: breaks
    /// hidden inside a multi-block move aren't visible to the filter.
    pub blocks_cant_break: HashSet<BlockKind>,
}

impl Default for Movements {
    fn default() -> Self {
        Self {
            can_dig: true,
            can_open_doors: false,
            allow_parkour: true,
            allow_sprinting: true,
            allow_free_motion: false,
            dont_create_flow: true,
            dont_mine_under_falling_block: true,
            max_drop_down: 4,
            blocks_to_avoid: HashSet::from([BlockKind::Fire, BlockKind::Cobweb, BlockKind::Lava]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
        }
    }
}

impl Movements {
    /// The [`PathfinderOpts`] for this config — `can_dig` maps onto
    /// `allow_mining`, and every goto runs [`movements_move`] so the
    /// avoid/cant-break/parkour constraints reach the search.
    ///
    /// `retry_on_no_path: false` matches upstream: one failed search is a
    /// terminal `path_update` (`noPath`/`timeout`), not an infinite retry.
    /// The engine's per-tick `recalculate_if_has_goal_but_no_path` would
    /// otherwise loop 5 s searches against unreachable goals forever and
    /// a `goto` could never reject — the goal drop is what lets the
    /// bridge report the failure (see [`bridge_events`]).
    pub(super) fn to_opts(&self) -> PathfinderOpts {
        PathfinderOpts::new()
            .successors_fn(movements_move)
            .allow_mining(self.can_dig)
            .retry_on_no_path(false)
    }
}

/// The `Movements` subset a successors fn can act on, read back through
/// The engine's `CustomPathfinderState` typemap — `SuccessorsFn` is a plain
/// `fn` that can't capture config, but `MovesCtx::custom_state` is handed
/// to it by the pathfinder itself.
#[derive(Clone)]
pub(super) struct MovementConstraints {
    allow_parkour: bool,
    blocks_to_avoid: Arc<HashSet<BlockKind>>,
    blocks_cant_break: Arc<HashSet<BlockKind>>,
}

impl From<&Movements> for MovementConstraints {
    fn from(m: &Movements) -> Self {
        Self {
            allow_parkour: m.allow_parkour,
            blocks_to_avoid: Arc::new(m.blocks_to_avoid.clone()),
            blocks_cant_break: Arc::new(m.blocks_cant_break.clone()),
        }
    }
}

/// The successors fn every facade goto uses: the engine's `default_move` set
/// minus parkour when disabled, with `blocks_to_avoid`/`blocks_cant_break`
/// applied to the produced edges. With no constraints installed it's
/// exactly `default_move` (the facade always installs them, so this is
/// only a safe default for raw engine callers).
pub(super) fn movements_move(ctx: &mut moves::MovesCtx, node: RelBlockPos) {
    let Some(constraints) = ctx.custom_state.get::<MovementConstraints>() else {
        moves::default_move(ctx, node);
        return;
    };
    moves::basic::basic_move(ctx, node);
    if constraints.allow_parkour {
        moves::parkour::parkour_move(ctx, node);
    }
    moves::uncommon::uncommon_move(ctx, node);

    if constraints.blocks_to_avoid.is_empty() && constraints.blocks_cant_break.is_empty() {
        return;
    }
    let world = ctx.world;
    ctx.edges.retain_mut(|edge| {
        // occupying `target` needs both the feet and head block free
        let feet = world.get_block_state(edge.movement.target);
        if cant_break(&constraints.blocks_cant_break, feet) {
            return false;
        }
        let head = world.get_block_state(edge.movement.target.up(1));
        if cant_break(&constraints.blocks_cant_break, head) {
            return false;
        }
        if constraints.blocks_to_avoid.contains(&BlockKind::from(feet)) {
            edge.cost += AVOID_STEP_COST;
        }
        true
    });
}

/// Whether moving onto `state` would mean breaking a `blocks_cant_break`
/// member: the engine wants to mine through it (solid and diggable) but the
/// config forbids it.
pub(super) fn cant_break(
    cant_break: &HashSet<BlockKind>,
    state: mineflayer_engine::block::BlockState,
) -> bool {
    !cant_break.is_empty()
        && moves::should_mine_block_state(state)
        && cant_break.contains(&BlockKind::from(state))
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
            blocks_to_avoid: HashSet::from([BlockKind::Cactus]),
            blocks_cant_break: HashSet::from([BlockKind::Chest]),
            ..Default::default()
        };
        let c = MovementConstraints::from(&m);
        assert!(!c.allow_parkour);
        assert!(c.blocks_to_avoid.contains(&BlockKind::Cactus));
        assert!(!c.blocks_to_avoid.contains(&BlockKind::Lava));
        assert!(c.blocks_cant_break.contains(&BlockKind::Chest));
    }
}
