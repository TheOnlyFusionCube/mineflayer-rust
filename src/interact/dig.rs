//! Dig machinery: the `dig` promise, generation bookkeeping, and the
//! in-span cancellation of engine mining for aborted digs. Split out of
//! `interact` — the promise wait/settle/abort rules are self-contained.

use std::time::{Duration, Instant};

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    BlockPos, Vec3,
    app::{App, Plugin},
    block::BlockTrait,
    core::{direction::Direction, game_type::GameMode, hit_result::BlockHitResult, tick::GameTick},
    ecs::{entity::Entity, prelude::Component, schedule::IntoScheduleConfigs, world::World},
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, Position, dimensions::EntityDimensions,
        inventory::Inventory, mining::get_mine_progress,
    },
    interact::pick::HitResultComponent,
    local_player::WorldHolder,
    mining::{
        MineBlockPos, MineBlockProgressEvent, MineProgress, Mining, MiningQueued, MiningSystems,
        StopMiningBlockEvent,
    },
    packet::game::SendGamePacketEvent,
    physics::{clip::traverse_blocks, collision::BlockWithShape},
    protocol::packets::game::s_player_action::{Action as PlayerAction, ServerboundPlayerAction},
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
    /// An aborted dig whose engine state may still be coming — the
    /// reconciliation pass cancels it in-span (see
    /// [`reconcile_aborted_dig`]).
    aborted_dig: Option<AbortedEngineDig>,
}

/// Engine state owed to an aborted dig generation.
///
/// `start_mining` only enqueues a message: at abort time the engine may
/// have no `MiningQueued`/`Mining` yet, and the message may materialize
/// and be acted on (including the engine's creative instant-break) before any
/// external task can look. [`reconcile_aborted_dig`] runs at the head of
/// the mining chain, so this is the earliest point the port can cancel
/// inside the tick that materializes the start.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AbortedEngineDig {
    pos: BlockPos,
    seq: u64,
    /// Consecutive reconciliation runs that found no engine state: a start
    /// cannot be pending past one full span, so two quiet runs are proof
    /// nothing more is coming.
    quiet_runs: u8,
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

    pub(crate) fn mark_aborted_engine_dig(&mut self, pos: BlockPos, seq: u64) {
        // a stale abort exit (dig A resuming after dig B was stopped) must
        // not clobber the newer generation's marker: the reconciliation
        // would discard it as superseded and B's pending start would
        // escape unguarded
        if self.dig_seq != seq {
            return;
        }
        self.aborted_dig = Some(AbortedEngineDig {
            pos,
            seq,
            quiet_runs: 0,
        });
    }
}

#[cfg(test)]
impl InteractShared {
    /// A shared whose current generation is `seq` — marker publication is
    /// gated on `dig_seq == seq`, so callers that only need a marker need
    /// the two to match.
    pub(crate) fn with_generation(seq: u64) -> Self {
        Self {
            dig_seq: seq,
            ..Default::default()
        }
    }
}

/// Cancel engine state owed to an aborted dig, at the head of `GameTick`'s
/// mining chain (installed by [`DigAbortPlugin`]).
///
/// A `StartMiningBlockEvent` written by `start_mining` is only read by the
/// next `Update`; an abort landing before that cannot see the state it
/// creates, and the `Update`+`GameTick` pair runs under one ECS lock, so no
/// external task can intervene. This pass runs before `handle_mining_queued`
/// in the same span, so a freshly materialized `MiningQueued` is dropped
/// before it can start mining — or, in creative mode, instantly break the
/// block. Markers are dropped once a newer dig owns the generation, once
/// the state was cancelled, or after two consecutive runs found nothing (a
/// start cannot stay unread past one full span).
pub(crate) fn reconcile_aborted_dig(world: &mut World) {
    let entities: Vec<Entity> = {
        let mut query = world.query::<(Entity, &InteractShared)>();
        query
            .iter(world)
            .filter(|(_, shared)| shared.aborted_dig.is_some())
            .map(|(entity, _)| entity)
            .collect()
    };
    for entity in entities {
        let (target, superseded) = {
            let Some(shared) = world.get::<InteractShared>(entity) else {
                continue;
            };
            let Some(aborted) = shared.aborted_dig else {
                continue;
            };
            (aborted.pos, shared.dig_seq != aborted.seq)
        };
        if superseded {
            // a newer dig owns the generation — even at the same position
            // its engine state must survive untouched
            if let Some(mut shared) = world.get_mut::<InteractShared>(entity) {
                shared.aborted_dig = None;
            }
            continue;
        }
        let (queued, mining) = (
            world.get::<MiningQueued>(entity).map(|q| q.position) == Some(target),
            world.get::<Mining>(entity).map(|m| m.pos) == Some(target),
        );
        if queued {
            world.entity_mut(entity).remove::<MiningQueued>();
        }
        if mining {
            // The engine's stop handler runs in `Update`, but this pass runs in
            // `GameTick` ahead of the mining chain: enqueueing the event
            // alone would let `continue_mining_block` progress (or insta-
            // finish) the block for the rest of this tick, so the handler's
            // effects are applied synchronously — same shape as
            // `handle_stop_mining_block_event`
            let abort_pos = world.get::<MineBlockPos>(entity).and_then(|pos| pos.0);
            world.entity_mut(entity).remove::<Mining>();
            if let Some(mut progress) = world.get_mut::<MineProgress>(entity) {
                progress.0 = 0.;
            }
            if let Some(abort_pos) = abort_pos {
                world.trigger(SendGamePacketEvent::new(
                    entity,
                    ServerboundPlayerAction {
                        action: PlayerAction::AbortDestroyBlock,
                        pos: abort_pos,
                        direction: Direction::Down,
                        seq: 0,
                    },
                ));
            }
            world.write_message(MineBlockProgressEvent {
                entity,
                position: abort_pos.unwrap_or(target),
                destroy_stage: None,
            });
        }
        let Some(mut shared) = world.get_mut::<InteractShared>(entity) else {
            continue;
        };
        let Some(mut aborted) = shared.aborted_dig else {
            continue;
        };
        if queued || mining {
            shared.aborted_dig = None;
        } else {
            aborted.quiet_runs += 1;
            shared.aborted_dig = (aborted.quiet_runs < 2).then_some(aborted);
        }
    }
}

/// Installs [`reconcile_aborted_dig`]; added by the bot bootstrap
/// (`BotBuilder::connect` mirrors the engine's `StartClientOpts::new` and adds
/// this plugin before the ECS runner starts).
pub(crate) struct DigAbortPlugin;

impl Plugin for DigAbortPlugin {
    fn build(&self, app: &mut App) {
        // only the order before `MiningSystems` matters; the exclusive system
        // is otherwise unordered on purpose (no ambiguity warning)
        app.add_systems(
            GameTick,
            reconcile_aborted_dig
                .before(MiningSystems)
                .ambiguous_with_all(),
        );
    }
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
        // sees the whole start — and marks the generation so the in-span
        // reconciliation cancels the queued message — or sees none of it)
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

        // an aborted dig must not leave the engine mining: publishing the
        // generation makes `reconcile_aborted_dig` cancel any queued or
        // live engine operation in the next `GameTick`, before the mining
        // chain can act on it. There is deliberately no completion barrier
        // here — the abort error may surface before the ECS cleanup lands,
        // and the in-span pass cannot be raced by the mining chain.
        if matches!(outcome, DigExit::Aborted(_)) {
            self.mark_aborted_engine_dig(pos, my_seq);
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

    /// Record the aborted generation for [`reconcile_aborted_dig`].
    fn mark_aborted_engine_dig(&self, pos: BlockPos, seq: u64) {
        let mut ecs = self.client.ecs.write();
        if let Some(mut shared) = ecs.get_mut::<InteractShared>(self.client.entity) {
            shared.mark_aborted_engine_dig(pos, seq);
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
            ecs.entity_mut(entity).remove::<MiningQueued>();
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
        let mut shutdown = self.shutdown.clone();
        // mark the initial value seen so only the sender's drop resolves
        let _ = shutdown.borrow_and_update();
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
                _ = shutdown.changed() => break DigExit::Disconnected,
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
    /// generation, clears the port-owned target, sends the engine's
    /// `StopMiningBlockEvent` (→ `AbortDestroyBlock` packet) and drops a
    /// stale `MiningQueued`.
    pub fn stop_digging(&self) {
        let entity = self.client.entity;
        let mut ecs = self.client.ecs.write();
        let mining = ecs.get::<Mining>(entity).map(|m| m.pos);
        let queued = ecs.get::<MiningQueued>(entity).map(|q| q.position);
        {
            let Some(mut shared) = ecs.get_mut::<InteractShared>(entity) else {
                return;
            };
            // upstream `if (!targetDigBlock) return` — but engine state
            // outlives the target (a timed-out dig), and that must stay
            // stoppable: only a fully idle stop is a no-op
            if shared.dig_target.is_none() && mining.is_none() && queued.is_none() {
                return;
            }
            if let Some((pos, _, seq)) = shared.dig_target {
                shared.mark_aborted_engine_dig(pos, seq);
            } else if let Some(pos) = queued.or(mining) {
                // leftover engine state with no target: tag it with the
                // current generation so the reconciliation can retire it
                let seq = shared.dig_seq;
                shared.mark_aborted_engine_dig(pos, seq);
            }
            shared.abort_current();
            shared.dig_target = None;
        }
        if mining.is_none() && queued.is_none() {
            return;
        }
        if queued.is_some() {
            ecs.entity_mut(entity).remove::<MiningQueued>();
        }
        if mining.is_some() {
            ecs.write_message(StopMiningBlockEvent { entity });
        }
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
        let bot = crate::Bot::new(client);
        bot.stop_digging();
        let ecs = bot.client().ecs.read();
        let shared = ecs.get::<InteractShared>(bot.client().entity).unwrap();
        assert_eq!(shared.aborted_seq, 0);
        assert!(shared.last_dig_time.is_none());
    }

    fn aborted_world(aborted: Option<AbortedEngineDig>, dig_seq: u64) -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<bevy_ecs::message::Messages<StopMiningBlockEvent>>();
        world.init_resource::<bevy_ecs::message::Messages<MineBlockProgressEvent>>();
        let entity = world
            .spawn(InteractShared {
                dig_seq,
                aborted_dig: aborted,
                ..Default::default()
            })
            .id();
        (world, entity)
    }

    fn aborted_marker(pos: BlockPos, seq: u64) -> Option<AbortedEngineDig> {
        Some(AbortedEngineDig {
            pos,
            seq,
            quiet_runs: 0,
        })
    }

    /// The reconciliation drops a materialized queued mine for the aborted
    /// generation before the mining chain can act on it (the pending-start
    /// window no outside-tick code can close).
    #[test]
    fn reconcile_drops_a_queued_start_for_the_aborted_generation() {
        use mineflayer_engine::mining::MiningQueued;

        let pos = BlockPos::new(9, 64, 9);
        let (mut world, entity) = aborted_world(aborted_marker(pos, 1), 1);
        world.entity_mut(entity).insert(MiningQueued {
            position: pos,
            direction: Direction::Down,
            force: true,
        });
        reconcile_aborted_dig(&mut world);
        assert!(world.get::<MiningQueued>(entity).is_none());
        assert!(
            world
                .get::<InteractShared>(entity)
                .unwrap()
                .aborted_dig
                .is_none(),
            "cancelled marker should be cleared"
        );
        // stop was enqueued for a live mine, not for the queued one
        let messages = world.resource::<bevy_ecs::message::Messages<StopMiningBlockEvent>>();
        assert_eq!(messages.len(), 0);
    }

    /// A live `Mining` at the aborted position is retired by the pass
    /// itself — Mining removed, progress reset, the abort packet sent —
    /// before the mining chain below can progress (or insta-finish) it in
    /// the same tick (the engine's own stop handler runs a schedule later).
    #[test]
    fn reconcile_retires_live_mining_in_span() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use mineflayer_engine::ecs::observer::On;

        let pos = BlockPos::new(4, 64, 5);
        let (mut world, entity) = aborted_world(aborted_marker(pos, 3), 3);
        world.entity_mut(entity).insert((
            Mining {
                pos,
                dir: Direction::Down,
                force: true,
            },
            MineProgress(0.4),
            MineBlockPos(Some(pos)),
        ));
        let aborts = Arc::new(AtomicUsize::new(0));
        let counter = aborts.clone();
        world.add_observer(move |_: On<SendGamePacketEvent>| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        reconcile_aborted_dig(&mut world);
        assert!(
            world.get::<Mining>(entity).is_none(),
            "Mining survived the pass"
        );
        assert_eq!(world.get::<MineProgress>(entity).unwrap().0, 0.);
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert_eq!(
            world
                .resource::<bevy_ecs::message::Messages<MineBlockProgressEvent>>()
                .len(),
            1
        );
        // a second run is quiet: the marker is gone and nothing new fires
        reconcile_aborted_dig(&mut world);
        assert_eq!(aborts.load(Ordering::SeqCst), 1);
        assert_eq!(
            world
                .resource::<bevy_ecs::message::Messages<MineBlockProgressEvent>>()
                .len(),
            1
        );
    }

    /// A stale abort exit (dig A resuming after dig B was stopped) must not
    /// clobber the newer generation's marker.
    #[test]
    fn marker_publication_yields_to_a_newer_generation() {
        let pos_a = BlockPos::new(1, 64, 1);
        let pos_b = BlockPos::new(2, 64, 2);
        let mut shared = InteractShared {
            dig_seq: 2,
            aborted_dig: aborted_marker(pos_b, 2),
            ..Default::default()
        };
        shared.mark_aborted_engine_dig(pos_a, 1);
        assert_eq!(shared.aborted_dig.map(|a| a.pos), Some(pos_b));
        assert_eq!(shared.aborted_dig.map(|a| a.seq), Some(2));
        // the owning generation re-marks freely
        shared.mark_aborted_engine_dig(pos_a, 2);
        assert_eq!(shared.aborted_dig.map(|a| a.pos), Some(pos_a));
    }

    /// A newer generation owns the position: its engine state must survive,
    /// and the stale marker is dropped.
    #[test]
    fn reconcile_leaves_a_newer_generation_alone() {
        use mineflayer_engine::mining::MiningQueued;

        let pos = BlockPos::new(2, 64, 2);
        let (mut world, entity) = aborted_world(aborted_marker(pos, 1), 2);
        world.entity_mut(entity).insert(MiningQueued {
            position: pos,
            direction: Direction::Down,
            force: true,
        });
        reconcile_aborted_dig(&mut world);
        assert!(world.get::<MiningQueued>(entity).is_some());
        assert!(
            world
                .get::<InteractShared>(entity)
                .unwrap()
                .aborted_dig
                .is_none()
        );
    }

    /// A pending start cannot stay unread past one full span: two quiet
    /// runs prove nothing more is coming and drop the marker.
    #[test]
    fn reconcile_expires_a_quiet_marker_after_two_runs() {
        let pos = BlockPos::new(6, 64, 6);
        let (mut world, entity) = aborted_world(aborted_marker(pos, 1), 1);
        reconcile_aborted_dig(&mut world);
        let marker = world.get::<InteractShared>(entity).unwrap().aborted_dig;
        assert_eq!(marker.map(|m| m.quiet_runs), Some(1));
        reconcile_aborted_dig(&mut world);
        assert!(
            world
                .get::<InteractShared>(entity)
                .unwrap()
                .aborted_dig
                .is_none()
        );
    }

    /// A stop must still reach engine state that outlived the target (a
    /// timed-out dig): with no target but a live `MiningQueued`, the stop
    /// is not a no-op and drops the queued mine.
    #[tokio::test]
    async fn stop_digging_reaches_engine_leftovers_without_a_target() {
        use mineflayer_engine::mining::MiningQueued;

        let client = crate::test_util::live_entity_client((
            InteractShared::default(),
            MiningQueued {
                position: BlockPos::new(1, 64, 2),
                direction: Direction::Down,
                force: true,
            },
        ));
        let bot = crate::Bot::new(client);
        bot.stop_digging();
        let ecs = bot.client().ecs.read();
        assert!(
            ecs.get::<MiningQueued>(bot.client().entity).is_none(),
            "stop left the queued engine mine behind"
        );
        assert!(
            ecs.get::<InteractShared>(bot.client().entity)
                .unwrap()
                .aborted_dig
                .is_some(),
            "stop did not publish the aborted generation for reconciliation"
        );
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
        let bot = crate::Bot::new(client);
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
        let bot = crate::Bot::new(client);
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
