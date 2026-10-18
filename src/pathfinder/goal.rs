//! The `goals.Goal*` family as one sum type, and the engine goal adapters
//! it maps to.

use super::*;

/// A `bot.pathfinder` goal — upstream's `goals.Goal*` family as one sum
/// type. Each variant names the engine goal it maps to.
#[derive(Clone, Debug)]
pub enum Goal {
    /// `goals.GoalNear(x, y, z, range)` → [`goals::RadiusGoal`].
    /// Distance is measured to block *centers* (the engine semantics — a
    /// half-block offset vs upstream's raw node coords, `diverges`).
    Near {
        /// The position to get near.
        pos: Vec3,
        /// How close is close enough (euclidean, block centers).
        radius: f32,
    },
    /// `goals.GoalBlock(x, y, z)` → [`goals::BlockPosGoal`].
    Block(BlockPos),
    /// `goals.GoalGetToBlock(x, y, z)` — stand directly adjacent to the
    /// block (upstream `isEnd`: `|dx| + |dz| + |dy<0?dy+1:dy| == 1`).
    /// Custom `GetToBlockGoal` — the engine's `ReachBlockPosGoal` is the
    /// *LookAtBlock* analog (reach/LOS, not adjacency).
    GetToBlock(BlockPos),
    /// `goals.GoalXZ(x, z)` → [`goals::XZGoal`].
    XZ {
        /// Target x coordinate.
        x: i32,
        /// Target z coordinate.
        z: i32,
    },
    /// `goals.GoalNearXZ(x, z, range)` → `NearXZGoal` (the engine has no
    /// xz-radius goal; ours mirrors upstream's octagonal heuristic and
    /// squared-range `isEnd`).
    NearXZ {
        /// Target x coordinate.
        x: i32,
        /// Target z coordinate.
        z: i32,
        /// How close is close enough (euclidean, xz-plane only).
        range: f32,
    },
    /// `goals.GoalY(y)` → [`goals::YGoal`].
    Y {
        /// Target y coordinate.
        y: i32,
    },
    /// `goals.GoalLookAtBlock(pos)` → [`goals::ReachBlockPosGoal`] at the
    /// default 4.5 reach — stand where a face of the block is visible.
    /// `diverges`: the engine raycasts to the block *center* rather than
    /// enumerating visible faces, and upstream's `options.reach`/
    /// `entityHeight` aren't plumbed.
    LookAtBlock(BlockPos),
    /// `goals.GoalBreakBlock(x, y, z)` → [`goals::ReachBlockPosGoal`].
    /// Upstream's is just `GoalLookAtBlock` (it positions the bot; it
    /// does not break the block — `bot.dig` does that, same here).
    BreakBlock(BlockPos),
    /// `goals.GoalInvert(goal)` — succeeds exactly where the inner goal
    /// fails. Negating the heuristic misleads A* the way the engine's
    /// deprecated `InverseGoal` did (`diverges`).
    Invert(Box<Goal>),
    /// `goals.GoalFollow(entity, range)` — the bot paths to a radius
    /// goal again whenever the entity's block position changes
    /// (`diverges`: re-paths on whole-block moves rather than upstream's
    /// continuous moving goal). Like upstream, `dynamic` is the
    /// `set_goal` flag, not a property of the goal — `dynamic = false`
    /// follows until the first in-range tick, `true` follows forever.
    Follow {
        /// The entity to follow.
        entity: EntityRef,
        /// Stop this far away from it.
        range: f32,
    },
    /// `goals.GoalCompositeAny(goals)` — succeed where any child succeeds;
    /// heuristic is the child minimum (`OrGoals` semantics).
    Any(Vec<Goal>),
    /// `goals.GoalCompositeAll(goals)` — succeed where every child
    /// succeeds; heuristic is the child maximum (`AndGoals`).
    All(Vec<Goal>),
}

impl Goal {
    /// The engine goal for pathing, or `None` for a [`Goal::Follow`]
    /// whose entity isn't loaded. A `Follow` resolves to a
    /// [`goals::RadiusGoal`] at the entity's *current* position — inside
    /// composites that's upstream's snapshot semantics (a composite never
    /// sees the entity's later moves; `diverges`). A top-level `Follow`
    /// is sent again each time the entity moves to another block. `chunks` is the bot's
    /// live chunk storage, needed only by the `ReachBlockPosGoal`-backed
    /// variants' line-of-sight check.
    pub(super) fn to_engine(&self, chunks: &ChunkStorage) -> Option<Arc<dyn EngineGoal>> {
        match self {
            Goal::Near { pos, radius } => Some(Arc::new(goals::RadiusGoal::new(*pos, *radius))),
            Goal::Block(pos) => Some(Arc::new(goals::BlockPosGoal(*pos))),
            Goal::GetToBlock(pos) => Some(Arc::new(GetToBlockGoal(*pos))),
            Goal::LookAtBlock(pos) | Goal::BreakBlock(pos) => Some(Arc::new(
                goals::ReachBlockPosGoal::new(*pos, chunks.clone()),
            )),
            Goal::XZ { x, z } => Some(Arc::new(goals::XZGoal { x: *x, z: *z })),
            Goal::NearXZ { x, z, range } => Some(Arc::new(NearXZGoal {
                x: *x,
                z: *z,
                range_sq: range * range,
            })),
            Goal::Y { y } => Some(Arc::new(goals::YGoal { y: *y })),
            Goal::Invert(inner) => inner
                .to_engine(chunks)
                .map(|goal| Arc::new(InvertedGoal(goal)) as Arc<dyn EngineGoal>),
            Goal::Any(children) => children
                .iter()
                .map(|child| child.to_engine(chunks))
                .collect::<Option<Vec<_>>>()
                .map(|goals| Arc::new(AnyGoal(goals)) as Arc<dyn EngineGoal>),
            Goal::All(children) => children
                .iter()
                .map(|child| child.to_engine(chunks))
                .collect::<Option<Vec<_>>>()
                .map(|goals| Arc::new(AllGoal(goals)) as Arc<dyn EngineGoal>),
            Goal::Follow { entity, range } => entity
                .get_component::<Position>()
                .map(|p| Arc::new(goals::RadiusGoal::new(**p, *range)) as Arc<dyn EngineGoal>),
        }
    }

    /// Whether [`Goal::to_engine`] actually reads its `chunks`
    /// parameter — only the `ReachBlockPosGoal`-backed variants
    /// raycast into chunk storage. Lets [`Goal::is_end`] skip the
    /// `WorldHolder` lock for every other goal shape (and stay usable
    /// where no world component exists).
    pub(super) fn needs_chunks(&self) -> bool {
        match self {
            Goal::LookAtBlock(_) | Goal::BreakBlock(_) => true,
            Goal::Invert(inner) => inner.needs_chunks(),
            Goal::Any(children) | Goal::All(children) => children.iter().any(Goal::needs_chunks),
            _ => false,
        }
    }

    /// The goal's own success check at `pos` — upstream's `goal.isEnd`.
    /// Evaluated through the engine goal this maps to so semantics can't
    /// drift: for [`Goal::Follow`] that's the `RadiusGoal` at the
    /// entity's current position — its `success` measures from the
    /// block *center* like the installed goal does (a raw Vec3 distance
    /// here would disagree by ±0.5 per axis), and it needs no
    /// `ChunkStorage`, so this arm skips `client.world()` entirely and
    /// is `false` while the entity isn't loaded.
    pub(super) fn is_end(&self, client: &mineflayer_engine::Client, pos: Vec3) -> bool {
        match self {
            Goal::Follow { entity, range } => entity.get_component::<Position>().is_some_and(|p| {
                goals::RadiusGoal::new(**p, *range).success(player_pos_to_block_pos(pos))
            }),
            _ => self
                .engine_goal(client)
                .is_some_and(|goal| goal.success(player_pos_to_block_pos(pos))),
        }
    }

    /// [`Goal::to_engine`] over the bot's chunks, locked only for the
    /// goals that read them.
    pub(super) fn engine_goal(
        &self,
        client: &mineflayer_engine::Client,
    ) -> Option<Arc<dyn EngineGoal>> {
        let chunks = if self.needs_chunks() {
            client.world().read().chunks.clone()
        } else {
            ChunkStorage::default()
        };
        self.to_engine(&chunks)
    }
}

/// Structural identity for the `newGoal !== goal` check upstream's
/// `goto` runs on `goal_updated` — a re-issued goal equal to the pending
/// one doesn't count as a replacement (upstream compares by object
/// identity; a value type's closest honest analog is equality, so this
/// errs toward *not* rejecting where upstream's identity check
/// wouldn't — including a freshly-constructed equal goal, which
/// upstream's `!==` WOULD reject; a real residual for callers like
/// collectBlock that rebuild goals per iteration). `dynamic` is not
/// part of the identity — upstream compares only the goal object.
/// [`Goal::Follow`] compares the target's ECS id — the `Client` inside
/// an `EntityRef` isn't part of the goal's meaning.
pub(super) fn same_goal(a: &Goal, b: &Goal) -> bool {
    match (a, b) {
        (
            Goal::Near {
                pos: p1,
                radius: r1,
            },
            Goal::Near {
                pos: p2,
                radius: r2,
            },
        ) => p1 == p2 && r1 == r2,
        (Goal::Block(a), Goal::Block(b)) => a == b,
        (Goal::GetToBlock(a), Goal::GetToBlock(b)) => a == b,
        (Goal::LookAtBlock(a), Goal::LookAtBlock(b)) => a == b,
        (Goal::BreakBlock(a), Goal::BreakBlock(b)) => a == b,
        (Goal::XZ { x: x1, z: z1 }, Goal::XZ { x: x2, z: z2 }) => x1 == x2 && z1 == z2,
        (
            Goal::NearXZ {
                x: x1,
                z: z1,
                range: r1,
            },
            Goal::NearXZ {
                x: x2,
                z: z2,
                range: r2,
            },
        ) => x1 == x2 && z1 == z2 && r1 == r2,
        (Goal::Y { y: y1 }, Goal::Y { y: y2 }) => y1 == y2,
        (Goal::Invert(a), Goal::Invert(b)) => same_goal(a, b),
        (
            Goal::Follow {
                entity: e1,
                range: r1,
            },
            Goal::Follow {
                entity: e2,
                range: r2,
            },
        ) => e1.id() == e2.id() && r1 == r2,
        (Goal::Any(a), Goal::Any(b)) | (Goal::All(a), Goal::All(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| same_goal(x, y))
        }
        _ => false,
    }
}

/// `GoalCompositeAny` — the engine's `OrGoals` needs homogeneous children;
/// ours are boxed trait objects.
#[derive(Debug)]
pub(super) struct AnyGoal(pub(super) Vec<Arc<dyn EngineGoal>>);
impl EngineGoal for AnyGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        self.0
            .iter()
            .map(|goal| goal.heuristic(n))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(f32::INFINITY)
    }
    fn success(&self, n: BlockPos) -> bool {
        self.0.iter().any(|goal| goal.success(n))
    }
}

/// `GoalCompositeAll` — the `AndGoals` analog.
#[derive(Debug)]
pub(super) struct AllGoal(pub(super) Vec<Arc<dyn EngineGoal>>);
impl EngineGoal for AllGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        self.0
            .iter()
            .map(|goal| goal.heuristic(n))
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(f32::INFINITY)
    }
    fn success(&self, n: BlockPos) -> bool {
        self.0.iter().all(|goal| goal.success(n))
    }
}

/// `GoalInvert` — the semantics of the engine's deprecated `InverseGoal`:
/// negated heuristic, negated success.
#[derive(Debug)]
pub(super) struct InvertedGoal(pub(super) Arc<dyn EngineGoal>);
impl EngineGoal for InvertedGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        -self.0.heuristic(n)
    }
    fn success(&self, n: BlockPos) -> bool {
        !self.0.success(n)
    }
}

/// `GoalGetToBlock` — upstream's strict-adjacency goal:
/// `|dx| + |dz| + |dy<0?dy+1:dy| == 1` (a block below counts one step
/// less because the feet block is inside it). The engine's
/// `ReachBlockPosGoal` answers a different question (can a face be
/// seen within reach), so this one is implemented by hand.
#[derive(Debug)]
pub(super) struct GetToBlockGoal(pub(super) BlockPos);
impl EngineGoal for GetToBlockGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        let dx = (n.x - self.0.x).abs() as f32;
        let dy = (n.y - self.0.y) as f32;
        let dz = (n.z - self.0.z).abs() as f32;
        // upstream: distanceXZ(dx, dz) + Math.abs(dy < 0 ? dy + 1 : dy),
        // with distanceXZ the octagonal form and the whole thing scaled
        // by the engine's COST_HEURISTIC like every engine goal
        let (straight, diagonal) = if dx < dz {
            (dz - dx, dx)
        } else {
            (dx - dz, dz)
        };
        (diagonal * std::f32::consts::SQRT_2 + straight + if dy < 0. { dy + 1. } else { dy }.abs())
            * pathfinder::costs::COST_HEURISTIC
    }
    fn success(&self, n: BlockPos) -> bool {
        let dx = (n.x - self.0.x).abs();
        let dy_raw = n.y - self.0.y;
        let dy = if dy_raw < 0 { dy_raw + 1 } else { dy_raw }.abs();
        let dz = (n.z - self.0.z).abs();
        dx + dz + dy == 1
    }
}

/// `GoalNearXZ` — the engine has no xz-radius goal. Heuristic is upstream's
/// `distanceXZ` (octagonal: `|dx-dz| + min(dx,dz)·√2`), scaled by the engine's
/// `COST_HEURISTIC` like `XZGoal`; `isEnd` is upstream's `dx²+dz² ≤
/// range²`.
#[derive(Debug)]
pub(super) struct NearXZGoal {
    x: i32,
    z: i32,
    range_sq: f32,
}
impl EngineGoal for NearXZGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        let x = (self.x - n.x).unsigned_abs() as f32;
        let z = (self.z - n.z).unsigned_abs() as f32;
        let (straight, diagonal) = if x < z { (z - x, x) } else { (x - z, z) };
        (diagonal * std::f32::consts::SQRT_2 + straight) * pathfinder::costs::COST_HEURISTIC
    }
    fn success(&self, n: BlockPos) -> bool {
        let dx = (self.x - n.x) as f32;
        let dz = (self.z - n.z) as f32;
        dx * dx + dz * dz <= self.range_sq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pathfinder::tests::*;

    /// Each facade goal maps to an engine goal whose `success`/`heuristic`
    /// matches upstream semantics at probe positions. `LookAtBlock`/
    /// `BreakBlock` need a world for their LOS raycast, and `Follow`
    /// needs a live `EntityRef` — all covered by the live test instead.
    #[test]
    fn goal_mapping_table() {
        let chunks = ChunkStorage::default();

        // GoalXZ ignores y entirely.
        let g = Goal::XZ { x: 5, z: 7 }.to_engine(&chunks).unwrap();
        assert!(g.success(BlockPos::new(5, -60, 7)));
        assert!(g.success(BlockPos::new(5, 300, 7)));
        assert!(!g.success(BlockPos::new(5, 64, 8)));

        // GoalGetToBlock is strict adjacency — |dx|+|dz|+|dy<0?dy+1:dy|
        // == 1: four side neighbors succeed, and the block *above* the
        // target counts (dy_raw=1 → 1), but the block *below* needs two
        // (dy_raw=-1 → |−1+1|=0 fails, dy_raw=-2 → |−2+1|=1 succeeds).
        // It needs no chunks — pure adjacency math, unlike the
        // ReachBlockPosGoal it replaced.
        let g = Goal::GetToBlock(BlockPos::new(4, 64, 4))
            .to_engine(&chunks)
            .unwrap();
        for pos in [
            BlockPos::new(5, 64, 4),
            BlockPos::new(3, 64, 4),
            BlockPos::new(4, 64, 5),
            BlockPos::new(4, 64, 3),
            BlockPos::new(4, 65, 4), // standing on it
        ] {
            assert!(g.success(pos), "expected success at {pos:?}");
        }
        for pos in [
            BlockPos::new(4, 64, 4), // inside
            BlockPos::new(5, 64, 5), // diagonal
            BlockPos::new(4, 66, 4), // two above
            BlockPos::new(6, 64, 4), // two away
            BlockPos::new(4, 63, 4), // directly below
        ] {
            assert!(!g.success(pos), "expected failure at {pos:?}");
        }
        assert!(g.success(BlockPos::new(4, 62, 4))); // dy_raw=-2 → 1

        // GoalY ignores x/z.
        let g = Goal::Y { y: 70 }.to_engine(&chunks).unwrap();
        assert!(g.success(BlockPos::new(1000, 70, -1000)));
        assert!(!g.success(BlockPos::new(1000, 71, -1000)));

        // GoalBlock is exact.
        let g = Goal::Block(BlockPos::new(1, 2, 3))
            .to_engine(&chunks)
            .unwrap();
        assert!(g.success(BlockPos::new(1, 2, 3)));
        assert!(!g.success(BlockPos::new(1, 2, 4)));

        // GoalNear = euclidean radius — measured from block *centers*
        // (the engine semantics; upstream compares raw node coords, a
        // half-block offset — noted as diverges in api-parity.md).
        let g = Goal::Near {
            pos: Vec3::new(0.5, 64.5, 0.5),
            radius: 2.0,
        }
        .to_engine(&chunks)
        .unwrap();
        assert!(g.success(BlockPos::new(1, 64, 1)));
        assert!(!g.success(BlockPos::new(2, 64, 2)));

        // Invert flips success and the heuristic's sign.
        let inner = Goal::XZ { x: 0, z: 0 };
        let inner_engine = inner.to_engine(&chunks).unwrap();
        let g = Goal::Invert(Box::new(inner)).to_engine(&chunks).unwrap();
        assert!(g.success(BlockPos::new(10, 64, 10)));
        assert!(!g.success(BlockPos::new(0, 64, 0)));
        assert_eq!(
            g.heuristic(BlockPos::new(3, 64, 4)),
            -inner_engine.heuristic(BlockPos::new(3, 64, 4))
        );

        // GoalNearXZ — upstream `distanceXZ` (octagonal: straight +
        // diagonal·√2) × COST_HEURISTIC, and `isEnd` dx²+dz² ≤ range²
        // measured on integer node coords, y ignored.
        let g = Goal::NearXZ {
            x: 10,
            z: 10,
            range: 5.,
        }
        .to_engine(&chunks)
        .unwrap();
        // dx=dz=10 → straight 0, diagonal 10 → 10·√2·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(0, 64, 0)),
            10. * std::f32::consts::SQRT_2 * pathfinder::costs::COST_HEURISTIC
        );
        // dx=6, dz=0 → straight 6, diagonal 0 → 6·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(4, 64, 10)),
            6. * pathfinder::costs::COST_HEURISTIC
        );
        // dx=6, dz=8 → diagonal 6, straight 2 → (6√2+2)·COST_HEURISTIC
        assert_eq!(
            g.heuristic(BlockPos::new(4, 64, 2)),
            (6. * std::f32::consts::SQRT_2 + 2.) * pathfinder::costs::COST_HEURISTIC
        );
        assert!(g.success(BlockPos::new(13, 64, 14))); // dx²+dz² = 9+16 = 25 ≤ 25
        assert!(g.success(BlockPos::new(13, 300, 14))); // y ignored
        assert!(!g.success(BlockPos::new(16, 64, 10))); // 36 > 25

        // Any = OR, All = AND.
        let any = Goal::Any(vec![Goal::XZ { x: 5, z: 5 }, Goal::Y { y: 70 }])
            .to_engine(&chunks)
            .unwrap();
        assert!(any.success(BlockPos::new(5, 64, 5)));
        assert!(any.success(BlockPos::new(9, 70, 9)));
        assert!(!any.success(BlockPos::new(9, 64, 9)));

        let all = Goal::All(vec![Goal::XZ { x: 5, z: 5 }, Goal::Y { y: 70 }])
            .to_engine(&chunks)
            .unwrap();
        assert!(all.success(BlockPos::new(5, 70, 5)));
        assert!(!all.success(BlockPos::new(5, 64, 5)));
        assert!(!all.success(BlockPos::new(9, 70, 9)));
    }

    /// The `newGoal !== goal` check's value type analog — same variant,
    /// same parameters. `Follow` compares the target's ECS id; a
    /// different entity is a different goal.
    #[test]
    fn same_goal_identity() {
        let a = Goal::Near {
            pos: Vec3::new(1., 2., 3.),
            radius: 4.,
        };
        assert!(same_goal(&a, &a.clone()));
        assert!(!same_goal(
            &a,
            &Goal::Near {
                pos: Vec3::new(1., 2., 3.),
                radius: 5.
            }
        ));
        assert!(!same_goal(&a, &Goal::Block(BlockPos::new(1, 2, 3))));
        assert!(!same_goal(
            &Goal::LookAtBlock(BlockPos::new(1, 2, 3)),
            &Goal::BreakBlock(BlockPos::new(1, 2, 3))
        ));

        let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
        let e1 = client.ecs.write().spawn(()).id();
        let e2 = client.ecs.write().spawn(()).id();
        let f1 = Goal::Follow {
            entity: client.entity_ref_for(e1),
            range: 2.,
        };
        assert!(same_goal(
            &f1,
            &Goal::Follow {
                entity: client.entity_ref_for(e1),
                range: 2.
            }
        ));
        assert!(!same_goal(
            &f1,
            &Goal::Follow {
                entity: client.entity_ref_for(e2),
                range: 2.
            }
        ));
    }

    /// `Goal::Follow`'s `is_end` must answer with the same
    /// block-center `RadiusGoal` check the installed goal runs — a raw
    /// Vec3 distance disagrees by ±0.5 per axis and would report a
    /// reached goal as still pending at the boundary.
    #[test]
    fn follow_is_end_uses_block_center() {
        let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
        // entity (0.4, 64.5, 0.4) vs block (0,64,0) center (0.5,64.5,
        // 0.5): ≈0.14 — in range at r=0.5. The old Vec3 check against
        // the bot's origin would measure ≈0.75 and disagree.
        let target_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(0.4, 64.5, 0.4)))
            .id();
        let goal = Goal::Follow {
            entity: client.entity_ref_for(target_entity),
            range: 0.5,
        };
        assert!(goal.is_end(&client, client.position()));

        // past the center radius → out of range
        let far_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(1.6, 64.5, 0.4)))
            .id();
        let far = Goal::Follow {
            entity: client.entity_ref_for(far_entity),
            range: 0.5,
        };
        assert!(!far.is_end(&client, client.position()));

        // a despawned target is never satisfied — upstream's null-target
        // follow can't be met either
        let mut ecs = client.ecs.write();
        ecs.entity_mut(far_entity).despawn();
        drop(ecs);
        assert!(!far.is_end(&client, client.position()));
    }

    /// A `Follow` nested inside a composite resolves to a `RadiusGoal`
    /// snapshot of the entity's position at conversion time — the
    /// composite converts instead of erroring, and an untracked entity
    /// fails the conversion (`issue` then rejects "isn't loaded").
    #[test]
    fn composite_follow_resolves_to_position_snapshot() {
        let client = pathfinder_client(Position::new(Vec3::new(0., 64., 0.)));
        let target_entity = client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(5., 64., 0.)))
            .id();
        let target = client.entity_ref_for(target_entity);

        // Any(Follow@5,64,0 r=3, Block@0,64,0) — succeeds within 3 of
        // the entity's current position OR exactly on the block.
        let goal = Goal::Any(vec![
            Goal::Follow {
                entity: target.clone(),
                range: 3.,
            },
            Goal::Block(BlockPos::new(0, 64, 0)),
        ]);
        let engine_goal = goal
            .to_engine(&ChunkStorage::default())
            .expect("composite with a tracked Follow must convert");
        assert!(engine_goal.success(BlockPos::new(6, 64, 0))); // near the entity
        assert!(engine_goal.success(BlockPos::new(0, 64, 0))); // on the block
        assert!(!engine_goal.success(BlockPos::new(10, 64, 0))); // neither

        // `Invert(Follow)` resolves too — same `to_engine` arm.
        let inverted = Goal::Invert(Box::new(Goal::Follow {
            entity: target,
            range: 2.,
        }))
        .to_engine(&ChunkStorage::default())
        .unwrap();
        assert!(!inverted.success(BlockPos::new(5, 64, 0)));

        // an entity without a position fails the conversion — `issue`
        // rejects it instead of aiming at a made-up spot.
        let untracked = client.ecs.write().spawn_empty().id();
        assert!(
            Goal::Any(vec![Goal::Follow {
                entity: client.entity_ref_for(untracked),
                range: 1.,
            }])
            .to_engine(&ChunkStorage::default())
            .is_none()
        );
    }
}
