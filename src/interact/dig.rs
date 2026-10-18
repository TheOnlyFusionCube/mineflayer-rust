//! Dig machinery: the `dig` promise. A dig is an operation in the
//! [`OpSlot`] of [`InteractShared`]: a newer dig or `stop_digging` cancels
//! it, and the engine's [`MiningFinished`] for its mining start ends it.

use std::time::{Duration, Instant};

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    BlockPos, Vec3,
    block::{BlockState, BlockTrait},
    core::{direction::Direction, game_type::GameMode, hit_result::BlockHitResult},
    ecs::{entity::Entity, prelude::Component, world::World},
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, inventory::Inventory,
        mining::get_mine_progress,
    },
    interact::pick::HitResultComponent,
    inventory::ItemStack,
    local_player::WorldHolder,
    mining::{
        CancelMiningEvent, Mining, MiningFinished, MiningId, MiningOutcome, MiningQueued,
        StartMiningBlockEvent,
    },
    physics::collision::BlockWithShape,
};
use tokio::sync::oneshot;

use super::ForceLook;
use crate::{
    Bot, BotEvent, Error, Result,
    op::{Op, OpSlot},
    world::Block,
};

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
/// promise; the engine's `Mining`/`MiningQueued` components lag that by up
/// to a tick, so the promise-visible target is owned here.
#[derive(Component, Default)]
pub(crate) struct InteractShared {
    /// The running dig.
    dig: OpSlot,
    /// `bot.targetDigBlock`/`targetDigFace` — the running dig's target and
    /// the face sent on the wire.
    dig_target: Option<(BlockPos, Direction)>,
    /// The running dig's mining start, and where its outcome goes.
    dig_mining: Option<(MiningId, oneshot::Sender<MiningOutcome>)>,
    /// `bot.lastDigTime` — set when a dig finishes for any reason.
    last_dig_time: Option<Instant>,
}

/// Gives each running dig the outcome of its own mining start.
pub(crate) struct DigPlugin;

impl mineflayer_engine::app::Plugin for DigPlugin {
    fn build(&self, app: &mut mineflayer_engine::app::App) {
        app.add_systems(mineflayer_engine::app::Update, settle_digs);
    }
}

fn settle_digs(
    mut finished: mineflayer_engine::ecs::message::MessageReader<MiningFinished>,
    mut digs: mineflayer_engine::ecs::system::Query<&mut InteractShared>,
) {
    for finished in finished.read() {
        let Ok(mut shared) = digs.get_mut(finished.entity) else {
            continue;
        };
        if shared
            .dig_mining
            .as_ref()
            .is_some_and(|(id, _)| *id == finished.id)
            && let Some((_, waiter)) = shared.dig_mining.take()
        {
            let _ = waiter.send(finished.outcome);
        }
    }
}

/// Trigger the engine's [`CancelMiningEvent`] and apply it before the ECS
/// lock is released, so the port's next read sees the cancelled state.
fn cancel_engine_mining(ecs: &mut World, entity: Entity) {
    ecs.trigger(CancelMiningEvent { entity });
    ecs.flush();
}

/// Mine progress per tick of `stack` on `state` for a player with these
/// components: the one scorer that dig time and the best-tool picks use.
/// Dig time is its inverse, so the fastest tool has the most progress. It
/// takes the components, not a `Bot`, so it runs inside an ECS query.
pub fn mine_progress(
    state: BlockState,
    stack: &ItemStack,
    fluid_on_eyes: &FluidOnEyes,
    physics: &Physics,
    attributes: &Attributes,
    active_effects: &ActiveEffects,
) -> f32 {
    get_mine_progress(
        Box::<dyn BlockTrait>::from(state).as_ref(),
        stack,
        fluid_on_eyes,
        physics,
        attributes,
        active_effects,
    )
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
        // start the operation, publish the target, and enqueue the start
        // under one lock: a `stop_digging` either sees the whole start (and
        // its cancel drops the queued message) or none of it
        let (op, mining) = {
            let mut ecs = self.client.ecs.write();
            let (op, id, mining) = {
                let Some(mut shared) = ecs.get_mut::<InteractShared>(self.client.entity) else {
                    return Err(Error::NotConnected);
                };
                let op = shared.dig.start(&self.root);
                let id = MiningId::next();
                let (waiter, mining) = oneshot::channel();
                shared.dig_target = Some((pos, target_face));
                shared.dig_mining = Some((id, waiter));
                (op, id, mining)
            };
            ecs.write_message(StartMiningBlockEvent {
                entity: self.client.entity,
                position: pos,
                force: true,
                id,
            });
            (op, mining)
        };

        let outcome = self
            .dig_wait(
                &op,
                mining,
                pos,
                tokio::time::Instant::now() + wait + DIG_FINISH_GRACE,
            )
            .await;
        self.settle_dig(&op, matches!(outcome, DigExit::Aborted(_)));
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

    /// Settle the promise-visible dig state when `op` ends: stamp
    /// `lastDigTime` (upstream's resolve-or-abort stamp), and, while `op`
    /// is still the running dig, clear `targetDigBlock` and stop the
    /// engine mining of an aborted dig. A newer dig or `stop_digging`
    /// owns that state otherwise.
    fn settle_dig(&self, op: &Op, aborted: bool) {
        let entity = self.client.entity;
        let mut ecs = self.client.ecs.write();
        let owned = {
            let Some(mut shared) = ecs.get_mut::<InteractShared>(entity) else {
                return;
            };
            shared.last_dig_time = Some(Instant::now());
            let owned = shared.dig.finish(op);
            if owned {
                shared.dig_target = None;
                shared.dig_mining = None;
            }
            owned
        };
        if owned && aborted {
            cancel_engine_mining(&mut ecs, entity);
        }
    }

    /// The dig wait after the start is queued: until the engine ends the
    /// mining start, the operation is cancelled, the deadline passes, or
    /// the connection ends. A mining that ends without a break still
    /// counts when the block is air (someone else broke it).
    async fn dig_wait(
        &self,
        op: &Op,
        mining: oneshot::Receiver<MiningOutcome>,
        pos: BlockPos,
        deadline: tokio::time::Instant,
    ) -> DigExit {
        tokio::select! {
            biased;
            _ = self.root.cancelled() => DigExit::Disconnected,
            _ = op.cancelled() => DigExit::Aborted(DigAbortReason::Stopped),
            outcome = mining => match outcome {
                Ok(MiningOutcome::Broken) => DigExit::Done,
                _ if self.block_is_air(pos) => DigExit::Done,
                _ => DigExit::Aborted(DigAbortReason::Interrupted),
            },
            _ = tokio::time::sleep_until(deadline) => DigExit::Aborted(DigAbortReason::Timeout),
        }
    }

    /// `bot.stopDigging()` — abort the in-flight dig. The pending `dig`
    /// future emits `diggingAborted` and rejects, exactly like upstream.
    ///
    /// A stop with no dig in flight is a no-op (upstream
    /// `if (!targetDigBlock) return`); the target is set before the wire
    /// call, so a stop landing in the `start_mining`→`MiningQueued` window
    /// still aborts the pending `dig`. Otherwise it cancels the dig,
    /// clears the port-owned target, and sends the engine's
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
            if shared.dig_target.take().is_some() {
                shared.last_dig_time = Some(Instant::now());
            }
            shared.dig.cancel();
            shared.dig_mining = None;
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
        let held = self
            .client
            .try_query_self::<&Inventory, _>(|inventory| inventory.held_item().clone())
            .ok()?;
        dig_time_ms(self.mine_progress(&held, block))
    }

    /// How much of `block` one game tick of mining with `stack` breaks
    /// (`1.` breaks it in one tick), with this bot's effects, water, and
    /// ground state; `0.` for an unbreakable block or when the player
    /// components are gone. Dig time is its inverse, so the best tool has
    /// the most progress.
    pub fn mine_progress(&self, stack: &ItemStack, block: &Block) -> f32 {
        self.client
            .try_query_self::<(&FluidOnEyes, &Physics, &Attributes, &ActiveEffects), _>(
                |(fluid_on_eyes, physics, attributes, effects)| {
                    mine_progress(
                        block.state,
                        stack,
                        fluid_on_eyes,
                        physics,
                        attributes,
                        effects,
                    )
                },
            )
            .unwrap_or(0.)
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
        let (pos, _) = self.port_dig_target()?;
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
    fn port_dig_target(&self) -> Option<(BlockPos, Direction)> {
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
        let Some((eye, _)) = self.eye_and_look() else {
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

    /// The first collision-shape clip along the ray; `None` past `range`
    /// or through unloaded chunks.
    fn raycast_hit(&self, from: Vec3, direction: Vec3, range: f64) -> Option<BlockHitResult> {
        let shared = self.client.get_component::<WorldHolder>()?.shared.clone();
        let world = shared.read();
        crate::world::raycast(&world, from, direction, range, |pos, state, to| {
            state.collision_shape().clip(from, to, pos)
        })
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
                id: MiningId::next(),
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

    /// `stopDigging` cancels the running dig, clears the port-owned target,
    /// and stamps `lastDigTime` in the same step (upstream clears
    /// `targetDigBlock` as it emits `diggingAborted`).
    #[tokio::test]
    async fn stop_digging_cancels_the_dig_and_clears_its_target() {
        let client = crate::test_util::live_entity_client(InteractShared {
            dig_target: Some((BlockPos::new(1, 64, 2), Direction::Up)),
            ..InteractShared::default()
        });
        let bot = crate::test_util::bot(client);
        let op = bot
            .client()
            .ecs
            .write()
            .get_mut::<InteractShared>(bot.client().entity)
            .unwrap()
            .dig
            .start(&bot.root);
        bot.stop_digging();
        assert!(op.is_cancelled());
        let ecs = bot.client().ecs.read();
        let shared = ecs.get::<InteractShared>(bot.client().entity).unwrap();
        assert!(shared.dig_target.is_none());
        assert!(shared.last_dig_time.is_some());
    }

    /// `targetDigFace` reads the face stored when the dig started.
    #[tokio::test]
    async fn target_dig_face_reads_the_stored_face() {
        let client = crate::test_util::live_entity_client(InteractShared {
            dig_target: Some((BlockPos::new(0, 64, 0), Direction::East)),
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

    /// Run `dig_wait` for a fresh operation, with `outcome` (if any) as
    /// the engine's answer.
    async fn wait_with(
        bot: &Bot,
        outcome: Option<MiningOutcome>,
        deadline: Duration,
        before: impl FnOnce(&Op),
    ) -> DigExit {
        let mut slot = OpSlot::default();
        let op = slot.start(&bot.root);
        let (waiter, mining) = oneshot::channel();
        if let Some(outcome) = outcome {
            waiter.send(outcome).unwrap();
        } else {
            std::mem::forget(waiter);
        }
        before(&op);
        let deadline = tokio::time::Instant::now() + deadline;
        tokio::time::timeout(
            Duration::from_secs(5),
            bot.dig_wait(&op, mining, BlockPos::new(1, 64, 1), deadline),
        )
        .await
        .expect("dig_wait hung")
    }

    #[tokio::test]
    async fn dig_wait_maps_each_exit() {
        let (bot, shutdown) =
            crate::test_util::bot_with_shutdown(crate::test_util::tick_client(()));
        let long = Duration::from_secs(60);
        assert!(matches!(
            wait_with(&bot, Some(MiningOutcome::Broken), long, |_| {}).await,
            DigExit::Done
        ));
        // the block is not air (there is no world): not a break
        assert!(matches!(
            wait_with(&bot, Some(MiningOutcome::Cancelled), long, |_| {}).await,
            DigExit::Aborted(DigAbortReason::Interrupted)
        ));
        assert!(matches!(
            wait_with(&bot, None, long, |op| op.token.cancel()).await,
            DigExit::Aborted(DigAbortReason::Stopped)
        ));
        assert!(matches!(
            wait_with(&bot, None, Duration::from_millis(20), |_| {}).await,
            DigExit::Aborted(DigAbortReason::Timeout)
        ));
        drop(shutdown);
        assert!(matches!(
            wait_with(&bot, None, long, |_| {}).await,
            DigExit::Disconnected
        ));
    }

    /// A dig through the real engine resolves on its own mining's
    /// `MiningFinished` and leaves no target behind.
    #[test]
    fn a_dig_in_the_engine_resolves_when_the_block_breaks() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };

        use mineflayer_engine::{
            block::BlockState, protocol::packets::game::ClientboundBlockUpdate,
            registry::builtin::BlockKind,
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let _rt = rt.enter();
        let mut sim = crate::test_util::sim::BotSim::new();
        let pos = sim.bot.position().to_block_pos_floor().down(1);
        sim.receive(ClientboundBlockUpdate {
            pos,
            block_state: BlockState::from(BlockKind::Dirt),
        });
        sim.tick();
        let block = sim.bot.block_at(pos).unwrap();
        let bot = sim.bot.clone();
        let mut dig = std::pin::pin!(bot.dig_opts(&block, ForceLook::Ignore, DigFace::Auto));
        let mut cx = Context::from_waker(Waker::noop());
        let mut result = None;
        for _ in 0..400 {
            if let Poll::Ready(r) = dig.as_mut().poll(&mut cx) {
                result = Some(r);
                break;
            }
            sim.tick();
        }
        assert!(matches!(result, Some(Ok(()))), "{result:?}");
        assert!(sim.bot.target_dig_block().is_none());
        assert!(sim.bot.last_dig_time().is_some());
    }
}
