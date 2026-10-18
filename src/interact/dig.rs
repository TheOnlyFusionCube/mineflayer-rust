//! Dig machinery: the `dig` promise and its generation bookkeeping. Split
//! out of `interact` — the promise wait/settle/abort rules are
//! self-contained.

use std::time::{Duration, Instant};

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    BlockPos, Vec3,
    block::BlockTrait,
    core::{direction::Direction, game_type::GameMode, hit_result::BlockHitResult},
    ecs::{entity::Entity, prelude::Component, world::World},
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, Position, dimensions::EntityDimensions,
        inventory::Inventory, mining::get_mine_progress,
    },
    interact::pick::HitResultComponent,
    local_player::WorldHolder,
    mining::{CancelMiningEvent, Mining, MiningQueued},
    physics::{clip::traverse_blocks, collision::BlockWithShape},
};
use tokio::sync::broadcast;

use super::ForceLook;
use crate::{Bot, BotEvent, Error, Result, world::Block};

/// Ticks allowed for a queued dig to reach `Mining` (~1s — the engine converts
/// `MiningQueued`→`Mining` on the next `GameTick`; anything longer means the
/// engine refused the dig).
const DIG_START_GRACE_TICKS: u32 = 20;

/// How long past `dig_time` to wait for the break to land client-side.
/// Upstream waits on the block update forever; surfacing a bounded wait as
/// [`DigAbortReason::Timeout`] beats hanging.
const DIG_FINISH_GRACE: Duration = Duration::from_secs(10);

/// `bot.dig`'s `digFace` argument (`lib/plugins/digging.js`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DigFace {
    /// `'auto'` — look at the block center and let the server pick the face.
    #[default]
    Auto,
    /// A vector — look at `center + 0.5·v` so the server sees that face.
    Vec(Vec3),
    /// `'raycast'` — trace from the eye to each visible face and look at the
    /// closest reachable intersection (the face used for `targetDigFace`).
    Raycast,
}

/// Why a [`Bot::dig`] future rejected — the `diggingAborted` payload.
///
/// Upstream emits a bare event and rejects with `'Digging aborted'`; the
/// variants name the causes we can actually distinguish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigAbortReason {
    /// `stop_digging` ran (or another `dig` superseded this one).
    Stopped,
    /// Mining stopped without breaking the block (death, engine removal, …).
    Interrupted,
    /// The block never became air within `dig_time + 10s` (upstream hangs).
    Timeout,
}

/// How a `dig` wait ended — `dig_opts` maps it onto the event + result.
enum DigExit {
    /// The block became air.
    Done,
    /// The dig aborted for the given reason (emits `diggingAborted`).
    Aborted(DigAbortReason),
    /// The connection ended mid-wait. Upstream's `diggingTask` parks
    /// forever here; the disconnect isn't an abort, so no `diggingAborted`
    /// is emitted — `End` already reported it.
    Disconnected,
}

/// Dig bookkeeping kept on the client entity.
///
/// upstream's `targetDigBlock`/`targetDigFace`/`lastDigTime` live in
/// `digging.js` and settle in the same synchronous step as the dig's
/// promise; the engine's `Mining`/`MiningQueued` components lag that by up to
/// a tick (an instant-break dig resolves while `MiningQueued` is still
/// queued), so the promise-visible target is owned here. `dig_seq`/
/// `aborted_seq` drive `stop_digging`'s generation check.
#[derive(Component, Default)]
pub(crate) struct InteractShared {
    /// Bumped every time `dig` starts — the current dig generation.
    dig_seq: u64,
    /// `dig_seq` value at the last `stop_digging`; digs with `seq <=` this
    /// have been aborted.
    aborted_seq: u64,
    /// `bot.targetDigBlock`/`targetDigFace` — the in-flight dig's target
    /// and the face sent on the wire, tagged with its `dig_seq` so a
    /// superseding dig's settle can't clear it.
    dig_target: Option<(BlockPos, Direction, u64)>,
    /// `bot.lastDigTime` — set when a dig finishes for any reason.
    last_dig_time: Option<Instant>,
}

impl InteractShared {
    /// Mark the in-flight dig generation aborted — called for every
    /// `stop_digging`, including the `start_mining`→`MiningQueued` window
    /// where no mining component exists yet (upstream sets `targetDigBlock`
    /// synchronously inside `dig()`, so a stop can never be swallowed).
    fn abort_current(&mut self) {
        if self.dig_seq > self.aborted_seq {
            self.aborted_seq = self.dig_seq;
            self.last_dig_time = Some(Instant::now());
        }
    }
}

/// Trigger the engine's [`CancelMiningEvent`] and apply it before the ECS
/// lock is released, so the port's next read sees the cancelled state.
fn cancel_engine_mining(ecs: &mut World, entity: Entity) {
    ecs.trigger(CancelMiningEvent { entity });
    ecs.flush();
}

/// Per-tick mine progress → the `digTime` contract: `∞` for unbreakable
/// (`progress <= 0`), otherwise `ceil(1/progress)` ticks in ms.
fn dig_time_ms(progress: f32) -> Option<Duration> {
    (progress > 0.).then(|| Duration::from_millis((progress.recip().ceil() * 50.) as u64))
}

/// Upstream `canDigBlock`'s reach test: eye (feet + 1.65) to block center
/// `<= 5.1`.
fn within_dig_reach(block_center: Vec3, feet: Vec3) -> bool {
    block_center.distance_to(feet.up(1.65)) <= 5.1
}

impl Bot {
    /// `bot.dig(block)` — dig with default options (`forceLook` waits a tick,
    /// `digFace` `'auto'`). Resolves once the block breaks; rejects with
    /// `'Digging aborted'` after emitting `diggingAborted` otherwise.
    ///
    /// # Errors
    ///
    /// `Error::NotConnected` before the bot has spawned;
    /// `Error::InvalidArgument` for an unbreakable/undiggable target;
    /// `Error::InvalidState` for an unreachable face or a bot that is not
    /// fully spawned; `Error::DigAborted` for a stopped/interrupted dig
    /// (`Display` is upstream's `Digging aborted`); `Error::Timeout` when the block never
    /// breaks (upstream would wait forever).
    ///
    /// ```no_run
    /// use mineflayer::prelude::*;
    ///
    /// # async fn example(bot: Bot) -> Result<()> {
    /// let below = bot.position().to_block_pos_floor().down(1);
    /// if let Some(block) = bot.block_at(below)
    ///     && bot.can_dig_block(&block)
    /// {
    ///     bot.dig(&block).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn dig(&self, block: &Block) -> Result<()> {
        self.dig_opts(block, ForceLook::default(), DigFace::default())
            .await
    }

    /// `bot.dig(block, forceLook, digFace)` — the full upstream signature.
    pub async fn dig_opts(
        &self,
        block: &Block,
        force_look: ForceLook,
        dig_face: DigFace,
    ) -> Result<()> {
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        // upstream throws when `digTime` is Infinity — the same check also
        // gates on the local-player components existing (spawned bot)
        let Some(wait) = self.dig_time(block) else {
            return Err(Error::InvalidArgument(format!(
                "dig time for {} is Infinity",
                block.name()
            )));
        };
        // The engine `unwrap`s HitResultComponent when handling the start
        // event — missing means the pick systems haven't initialized yet,
        // not that we're offline
        if self.client.get_component::<HitResultComponent>().is_none() {
            return Err(Error::InvalidState(
                "bot isn't fully spawned yet".to_string(),
            ));
        }
        let force = force_look == ForceLook::Force;
        if force_look != ForceLook::Ignore {
            match dig_face {
                DigFace::Vec(face) if face.x != 0. || face.y != 0. || face.z != 0. => {
                    self.look_at(block.pos.center() + face * 0.5, force).await?;
                }
                DigFace::Raycast => self.dig_raycast_look(block, force).await?,
                _ => self.look_at(block.pos.center(), force).await?,
            }
        }
        // abort whatever the bot was digging before (upstream stopDigging)
        self.stop_digging();

        let pos = block.pos;
        // upstream sets `targetDigBlock`/`targetDigFace` synchronously at
        // dig start; the face is the one the engine will send — the hit result
        // when we're looking at the block, `Down` otherwise (its
        // `handle_start_mining_block_event` rule)
        let target_face = self
            .client
            .get_component::<HitResultComponent>()
            .and_then(|h| h.as_block_hit_result_if_not_miss().cloned())
            .filter(|b| b.block_pos == pos)
            .map(|b| b.direction)
            .unwrap_or(Direction::Down);
        // bump the generation, publish the target, and enqueue the start
        // under one lock: a `stop_digging` can then never land between the
        // target becoming visible and the wire call existing (it either
        // sees the whole start — and its cancel drops the queued message —
        // or sees none of it)
        let my_seq = {
            let mut ecs = self.client.ecs.write();
            let seq = {
                let Some(mut shared) = ecs.get_mut::<InteractShared>(self.client.entity) else {
                    return Err(Error::NotConnected);
                };
                shared.dig_seq += 1;
                let seq = shared.dig_seq;
                shared.dig_target = Some((pos, target_face, seq));
                seq
            };
            ecs.write_message(mineflayer_engine::mining::StartMiningBlockEvent {
                entity: self.client.entity,
                position: pos,
                force: true,
            });
            seq
        };

        let outcome = self
            .dig_wait(
                pos,
                my_seq,
                tokio::time::Instant::now() + wait + DIG_FINISH_GRACE,
            )
            .await;

        // an aborted dig must not leave the engine mining
        if matches!(outcome, DigExit::Aborted(_)) {
            self.cancel_engine_dig(my_seq);
        }
        self.settle_dig(pos, my_seq);
        match outcome {
            DigExit::Done => {
                let new_block = self.block_at(pos).unwrap_or_else(|| block.clone());
                self.emit_event(BotEvent::DiggingCompleted(new_block));
                Ok(())
            }
            DigExit::Aborted(DigAbortReason::Timeout) => {
                self.emit_event(BotEvent::DiggingAborted {
                    block: block.clone(),
                    reason: DigAbortReason::Timeout,
                });
                Err(Error::Timeout)
            }
            DigExit::Aborted(reason) => {
                self.emit_event(BotEvent::DiggingAborted {
                    block: block.clone(),
                    reason,
                });
                Err(Error::DigAborted(reason))
            }
            DigExit::Disconnected => Err(Error::NotConnected),
        }
    }

    /// Cancel the engine's queued or live mining for generation `seq`,
    /// unless a newer dig owns the engine state now. The check and the
    /// cancel share one lock with `dig`'s start, so a newer start is
    /// either seen here or written after the cancel, which leaves it alone.
    fn cancel_engine_dig(&self, seq: u64) {
        let mut ecs = self.client.ecs.write();
        if ecs
            .get::<InteractShared>(self.client.entity)
            .is_some_and(|s| s.dig_seq == seq)
        {
            cancel_engine_mining(&mut ecs, self.client.entity);
        }
    }

    /// Settle the promise-visible dig state when a `dig` future exits:
    /// clear the port-owned `targetDigBlock` (only while it is still this
    /// dig's — a superseding `dig` owns it otherwise), stamp
    /// `lastDigTime` (upstream's resolve-or-abort stamp), and drop a
    /// queued-but-never-started mine for this block so the engine cannot begin
    /// mining the (now broken) cell on the next tick.
    fn settle_dig(&self, pos: BlockPos, my_seq: u64) {
        let entity = self.client.entity;
        let mut ecs = self.client.ecs.write();
        let owned = {
            let Some(mut shared) = ecs.get_mut::<InteractShared>(entity) else {
                return;
            };
            shared.last_dig_time = Some(Instant::now());
            let owned = shared.dig_target.is_some_and(|(_, _, seq)| seq == my_seq);
            if owned {
                shared.dig_target = None;
            }
            owned
        };
        if owned
            && ecs.get::<Mining>(entity).is_none()
            && ecs.get::<MiningQueued>(entity).map(|q| q.position) == Some(pos)
        {
            cancel_engine_mining(&mut ecs, entity);
        }
    }

    /// The dig wait after `start_mining` is on the wire: poll each tick
    /// until the block turns air, the dig aborts, the deadline hits, or
    /// the connection ends. Split from `dig_opts` so tests can drive it —
    /// the `HitResultComponent` it gates on can't be constructed outside
    /// The engine.
    ///
    /// The `shutdown` and `sleep_until` arms carry the exits ticks can't:
    /// `TickBroadcast`'s sender lives in the ECS `World` kept alive by
    /// `self.client`, so `recv` never sees `Closed` on disconnect, and a
    /// stalled tick stream would park the deadline check forever — the
    /// deadline must resolve without a tick arriving.
    async fn dig_wait(
        &self,
        pos: BlockPos,
        my_seq: u64,
        deadline: tokio::time::Instant,
    ) -> DigExit {
        let mut ticks = self.client.get_tick_broadcaster();
        let root = self.root.clone();
        let mut started = false;
        let mut start_grace = DIG_START_GRACE_TICKS;
        // once `Mining` disappears the finish (or an engine abort) has been
        // sent; the air update still needs ~1 RTT — give it a short grace
        // before declaring the dig interrupted
        let mut finish_grace = DIG_START_GRACE_TICKS;
        loop {
            let (target, aborted) = {
                let ecs = self.client.ecs.read();
                let entity = self.client.entity;
                let target = ecs
                    .get::<Mining>(entity)
                    .map(|m| (m.pos, m.dir))
                    .or_else(|| {
                        ecs.get::<MiningQueued>(entity)
                            .map(|q| (q.position, q.direction))
                    });
                let aborted = ecs
                    .get::<InteractShared>(entity)
                    .is_some_and(|s| s.aborted_seq >= my_seq);
                (target, aborted)
            };
            if aborted {
                break DigExit::Aborted(DigAbortReason::Stopped);
            }
            match target {
                Some((p, _)) if p != pos => break DigExit::Aborted(DigAbortReason::Stopped),
                Some(_) => started = true,
                None => {
                    if started {
                        finish_grace = finish_grace.saturating_sub(1);
                        if finish_grace == 0 {
                            break DigExit::Aborted(DigAbortReason::Interrupted);
                        }
                    } else {
                        start_grace = start_grace.saturating_sub(1);
                        if start_grace == 0 {
                            break DigExit::Aborted(DigAbortReason::Interrupted);
                        }
                    }
                }
            }
            if self.block_is_air(pos) {
                break DigExit::Done;
            }
            tokio::select! {
                r = ticks.recv() => {
                    match r {
                        // capacity-1 broadcast — a scheduling stall loses
                        // ticks, not the dig; only a dead channel interrupts
                        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            break DigExit::Aborted(DigAbortReason::Interrupted);
                        }
                    }
                }
                _ = root.cancelled() => break DigExit::Disconnected,
                _ = tokio::time::sleep_until(deadline) => {
                    break DigExit::Aborted(DigAbortReason::Timeout);
                }
            }
        }
    }

    /// `bot.stopDigging()` — abort the in-flight dig. The pending `dig`
    /// future emits `diggingAborted` and rejects, exactly like upstream.
    ///
    /// A stop with no dig in flight is a no-op (upstream
    /// `if (!targetDigBlock) return`); the target is set before the wire
    /// call, so a stop landing in the `start_mining`→`MiningQueued` window
    /// still aborts the pending `dig`. Otherwise it stamps the abort
    /// generation, clears the port-owned target, and sends the engine's
    /// `CancelMiningEvent` (drops a queued start, aborts live mining with
    /// an `AbortDestroyBlock` packet).
    pub fn stop_digging(&self) {
        let entity = self.client.entity;
        let mut ecs = self.client.ecs.write();
        let engine_busy =
            ecs.get::<Mining>(entity).is_some() || ecs.get::<MiningQueued>(entity).is_some();
        {
            let Some(mut shared) = ecs.get_mut::<InteractShared>(entity) else {
                return;
            };
            // upstream `if (!targetDigBlock) return` — but engine state
            // outlives the target (a timed-out dig), and that must stay
            // stoppable: only a fully idle stop is a no-op
            if shared.dig_target.is_none() && !engine_busy {
                return;
            }
            shared.abort_current();
            shared.dig_target = None;
        }
        cancel_engine_mining(&mut ecs, entity);
    }

    /// `bot.digTime(block)` — how long a dig takes with the held item,
    /// effects, and ground state. `None` is upstream's `Infinity`
    /// (unbreakable or the bot hasn't spawned); creative mode instamines.
    pub fn dig_time(&self, block: &Block) -> Option<Duration> {
        if self.game_mode() == Some(GameMode::Creative) {
            return Some(Duration::ZERO);
        }
        let block_trait = Box::<dyn BlockTrait>::from(block.state);
        let progress = self
            .client
            .try_query_self::<(
                &Inventory,
                &FluidOnEyes,
                &Physics,
                &Attributes,
                &ActiveEffects,
            ), _>(|(inventory, fluid_on_eyes, physics, attributes, effects)| {
                get_mine_progress(
                    block_trait.as_ref(),
                    inventory.held_item(),
                    fluid_on_eyes,
                    physics,
                    attributes,
                    effects,
                )
            })
            .ok()?;
        dig_time_ms(progress)
    }

    /// `bot.canDigBlock(block)` — diggable material within the survival
    /// reach of the eye (≈5.1 to the block center).
    pub fn can_dig_block(&self, block: &Block) -> bool {
        block.diggable() && within_dig_reach(block.pos.center(), self.position())
    }

    /// `bot.targetDigBlock` — the block the in-flight dig is mining, if
    /// any. Set when `dig` starts and cleared in the same step the dig's
    /// future settles, like upstream's `digging.js`.
    pub fn target_dig_block(&self) -> Option<Block> {
        let (pos, _, _) = self.port_dig_target()?;
        self.block_at(pos)
    }

    /// `bot.targetDigFace` — the face the dig is pointed at (`0..=5`,
    /// same ordinals as upstream's `block.face`).
    pub fn target_dig_face(&self) -> Option<Direction> {
        Some(self.port_dig_target()?.1)
    }

    /// `bot.lastDigTime` — when the last dig finished or aborted.
    pub fn last_dig_time(&self) -> Option<Instant> {
        self.client
            .get_component::<InteractShared>()
            .and_then(|s| s.last_dig_time)
    }

    /// The port-owned dig target — see [`InteractShared::dig_target`].
    fn port_dig_target(&self) -> Option<(BlockPos, Direction, u64)> {
        let ecs = self.client.ecs.read();
        ecs.get::<InteractShared>(self.client.entity)?.dig_target
    }

    fn block_is_air(&self, pos: BlockPos) -> bool {
        self.world()
            .and_then(|w| w.get_block_state(pos))
            .map(|s| s.is_air())
            .unwrap_or(false)
    }

    /// `digFace:'raycast'` — trace to each face of `block` visible from the
    /// eye and look at the closest reachable intersection; fall back to the
    /// center only when nothing obstructs the view and the block has no
    /// collision shape (tall grass), else `'Block not in view'`.
    async fn dig_raycast_look(&self, block: &Block, force: bool) -> Result<()> {
        let Some(eye) = self.dig_eye_pos() else {
            return Err(Error::NotConnected);
        };
        let center = block.pos.center();
        let feet = self.position();
        // visibleFaces: the axes where eye-to-center exceeds half a block
        let delta = Vec3 {
            x: feet.x - center.x,
            y: eye.y - center.y,
            z: feet.z - center.z,
        };
        let faces = [
            (delta.y.abs() > 0.5, delta.y.signum(), Direction::Up),
            (delta.x.abs() > 0.5, delta.x.signum(), Direction::East),
            (delta.z.abs() > 0.5, delta.z.signum(), Direction::South),
        ];
        let mut valid: Vec<Vec3> = Vec::new();
        let mut obstructed = false;
        for (visible, sign, positive_face) in faces {
            if !visible {
                continue;
            }
            let axis = match positive_face {
                Direction::Up => Vec3::new(0., sign, 0.),
                Direction::East => Vec3::new(sign, 0., 0.),
                _ => Vec3::new(0., 0., sign),
            };
            let target = center + axis * 0.5;
            let Some(hit) = self.raycast_hit(eye, target - eye, 5.) else {
                continue;
            };
            if hit.location.distance_to(eye) < target.distance_to(eye) {
                // another block clipped the ray before this face
                obstructed = true;
                continue;
            }
            if hit.block_pos == block.pos {
                valid.push(hit.location);
            }
        }
        if let Some(point) = valid.into_iter().min_by(|a, b| {
            a.distance_squared_to(eye)
                .partial_cmp(&b.distance_squared_to(eye))
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            return self.look_at(point, force).await;
        }
        if !obstructed && block.state.collision_shape().is_empty() {
            return self.look_at(center, force).await;
        }
        Err(Error::InvalidState("Block not in view".to_string()))
    }

    /// Eye position at feet + `EntityDimensions::eye_height` (the same
    /// accessor `entity_at_cursor` uses internally).
    fn dig_eye_pos(&self) -> Option<Vec3> {
        self.client
            .try_query_self::<(&Position, &EntityDimensions), _>(|(p, d)| p.up(d.eye_height as f64))
            .ok()
    }

    /// Block-collision raycast — same machinery as `entity_at_cursor`'s
    /// block pass: first collision-shape clip along the ray, `None` past
    /// `range` or through unloaded chunks.
    fn raycast_hit(&self, from: Vec3, direction: Vec3, range: f64) -> Option<BlockHitResult> {
        if range <= 0. || direction.length_squared() == 0. {
            return None;
        }
        let shared = self.client.get_component::<WorldHolder>()?.shared.clone();
        let world = shared.read();
        let to = from + direction * (range / direction.length());
        traverse_blocks(
            from,
            to,
            (),
            |_, pos| {
                let state = world.chunks.get_block_state(pos)?;
                state.collision_shape().clip(from, to, pos).map(Some)
            },
            |_| None,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// upstream `stopDigging` is a no-op when no dig is in flight
    /// (`if (!bot.targetDigBlock) return`).
    #[tokio::test]
    async fn stop_digging_without_a_target_is_a_no_op() {
        let client = crate::test_util::live_entity_client(InteractShared::default());
        let bot = crate::test_util::bot(client);
        bot.stop_digging();
        let ecs = bot.client().ecs.read();
        let shared = ecs.get::<InteractShared>(bot.client().entity).unwrap();
        assert_eq!(shared.aborted_seq, 0);
        assert!(shared.last_dig_time.is_none());
    }

    /// A stop must still reach engine state that outlived the target (a
    /// timed-out dig): with no target but a live `MiningQueued`, the stop
    /// is not a no-op and cancels the engine mining.
    #[tokio::test]
    async fn stop_digging_reaches_engine_leftovers_without_a_target() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use mineflayer_engine::ecs::observer::On;

        let client = crate::test_util::live_entity_client((
            InteractShared::default(),
            MiningQueued {
                position: BlockPos::new(1, 64, 2),
                direction: Direction::Down,
                force: true,
            },
        ));
        let cancels = Arc::new(AtomicUsize::new(0));
        let counter = cancels.clone();
        client
            .ecs
            .write()
            .add_observer(move |_: On<CancelMiningEvent>| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        let bot = crate::test_util::bot(client);
        bot.stop_digging();
        assert_eq!(cancels.load(Ordering::SeqCst), 1);
    }

    /// The in-flight dig's target is owned by the port; `stopDigging`
    /// clears it and stamps the abort generation in the same step
    /// (upstream clears `targetDigBlock` as it emits `diggingAborted`).
    #[tokio::test]
    async fn stop_digging_clears_the_port_owned_target() {
        let client = crate::test_util::live_entity_client(InteractShared {
            dig_seq: 2,
            dig_target: Some((BlockPos::new(1, 64, 2), Direction::Up, 2)),
            ..InteractShared::default()
        });
        let bot = crate::test_util::bot(client);
        bot.stop_digging();
        let ecs = bot.client().ecs.read();
        let shared = ecs.get::<InteractShared>(bot.client().entity).unwrap();
        assert!(shared.dig_target.is_none());
        assert_eq!(shared.aborted_seq, 2);
        assert!(shared.last_dig_time.is_some());
    }

    /// `targetDigFace` reads the face stored when the dig started.
    #[tokio::test]
    async fn target_dig_face_reads_the_stored_face() {
        let client = crate::test_util::live_entity_client(InteractShared {
            dig_target: Some((BlockPos::new(0, 64, 0), Direction::East, 1)),
            ..InteractShared::default()
        });
        let bot = crate::test_util::bot(client);
        assert_eq!(bot.target_dig_face(), Some(Direction::East));
    }

    #[test]
    fn dig_time_conversion() {
        // stone with a hand ≈ 0.0444/tick → 23 ticks = 1150ms
        let stone_hand = dig_time_ms(1. / 22.5).expect("breakable");
        assert_eq!(stone_hand, Duration::from_millis(1150));
        // instant-ish break → 1 tick
        assert_eq!(dig_time_ms(1.0), Some(Duration::from_millis(50)));
        // unbreakable (bedrock returns 0 progress) → Infinity → None
        assert_eq!(dig_time_ms(0.0), None);
        assert_eq!(dig_time_ms(-1.), None);
    }

    #[test]
    fn can_dig_reach_boundaries() {
        let feet = Vec3::new(0., 64., 0.);
        // eye = feet + 1.65; distance is measured to the block center
        let below_eye = |d: f64| Vec3::new(0., 64. + 1.65 - d, 0.);
        assert!(within_dig_reach(below_eye(5.0), feet));
        assert!(within_dig_reach(below_eye(5.09), feet));
        assert!(!within_dig_reach(below_eye(5.11), feet));
        assert!(!within_dig_reach(below_eye(5.2), feet));
    }

    #[test]
    fn abort_stamps_generation_without_mining_components() {
        // the start_mining→MiningQueued window: dig_seq is bumped but no
        // mining component exists yet — an abort must still land so the
        // pending `dig` sees it
        let mut shared = InteractShared {
            dig_seq: 3,
            ..Default::default()
        };
        shared.abort_current();
        assert_eq!(shared.aborted_seq, 3);
        assert!(shared.last_dig_time.is_some());
        // no un-aborted generation → idempotent no-op
        let stamped = shared.last_dig_time;
        shared.abort_current();
        assert_eq!(shared.aborted_seq, 3);
        assert_eq!(shared.last_dig_time, stamped);
    }

    /// The dig wait must not park on disconnect: `TickBroadcast`'s sender
    /// is kept alive by `self.client`, so `Closed` never arrives — the
    /// bridge's shutdown drop is the only exit, the same shape the pvp
    /// driver uses.
    #[tokio::test]
    async fn dig_wait_exits_on_shutdown() {
        let client = crate::test_util::tick_client(());
        let (bot, shutdown_tx) = crate::test_util::bot_with_shutdown(client);
        let pos = BlockPos::new(1, 64, 1);
        // parked mid-wait: no ticks arrive, no mining components — the
        // state the dig loop sits in after `start_mining`
        let wait = tokio::spawn({
            let bot = bot.clone();
            async move {
                bot.dig_wait(
                    pos,
                    1,
                    tokio::time::Instant::now() + Duration::from_secs(60),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        drop(shutdown_tx);
        let exit = tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .expect("dig_wait outlived the connection")
            .unwrap();
        assert!(matches!(exit, DigExit::Disconnected));
    }

    /// A stalled tick stream can't stall the deadline: `sleep_until`
    /// resolves without a tick arriving.
    #[tokio::test]
    async fn dig_wait_deadline_fires_without_ticks() {
        let client = crate::test_util::tick_client(());
        let (bot, _shutdown_tx) = crate::test_util::bot_with_shutdown(client);
        let pos = BlockPos::new(1, 64, 1);
        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            bot.dig_wait(
                pos,
                1,
                tokio::time::Instant::now() + Duration::from_millis(50),
            ),
        )
        .await
        .expect("deadline never fired");
        assert!(matches!(exit, DigExit::Aborted(DigAbortReason::Timeout)));
    }
}
