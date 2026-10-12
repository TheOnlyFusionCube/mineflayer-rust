//! Block interaction — digging, placing, and activating blocks plus
//! placing entities (`lib/plugins/digging.js`, `place_block.js`,
//! `generic_place.js`, `place_entity.js`, `inventory.js#activateBlock`).
//!
//! azalea owns the wire mechanics: `Client::mine`-equivalent start +
//! `Mining`/`MiningQueued` components, `ServerboundUseItemOn` for block
//! interactions, and the mine-progress formula behind `dig_time`. This module
//! layers the mineflayer contract on top:
//!
//! - `dig` looks first, aborts any previous dig, then resolves when the block
//!   becomes air and rejects with `DiggingAborted` otherwise.
//! - `place_block`/`place_entity` go through the upstream
//!   `_placeBlockWithOptions`/`_genericPlace` flow and wait for a
//!   `blockUpdate` on the reference *and* destination blocks before
//!   resolving, then emit `blockPlaced`/`entityPlaced`.
//! - `activate_block` sends the right-click interaction (which doubles as a
//!   placement trigger, exactly like upstream).
//! - `InteractShared` holds the bookkeeping upstream keeps in JS-side
//!   state (`lastDigTime`, abort coordination); azalea's `Mining` component
//!   is the `targetDigBlock`/`targetDigFace` source of truth.

use std::time::{Duration, Instant};

// `azalea::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use azalea::ecs as bevy_ecs;
use azalea::{
    BlockPos, Vec3,
    block::BlockTrait,
    core::{direction::Direction, game_type::GameMode, hit_result::BlockHitResult},
    ecs::prelude::Component,
    entity::{
        ActiveEffects, Attributes, FluidOnEyes, Physics, Position, dimensions::EntityDimensions,
        inventory::Inventory, mining::get_mine_progress,
    },
    interact::{BlockStatePredictionHandler, pick::HitResultComponent},
    local_player::WorldHolder,
    mining::{Mining, MiningQueued, StopMiningBlockEvent},
    physics::{clip::traverse_blocks, collision::BlockWithShape},
    protocol::packets::game::{
        s_interact::InteractionHand,
        s_use_item::ServerboundUseItem,
        s_use_item_on::{BlockHit, ServerboundUseItemOn},
    },
};
use tokio::sync::broadcast;

use crate::{Bot, BotEvent, Error, Result, world::Block};

/// Ticks allowed for a queued dig to reach `Mining` (~1s — azalea converts
/// `MiningQueued`→`Mining` on the next `GameTick`; anything longer means the
/// engine refused the dig).
const DIG_START_GRACE_TICKS: u32 = 20;

/// How long past `dig_time` to wait for the break to land client-side.
/// Upstream waits on the block update forever; surfacing a bounded wait as
/// [`DigAbortReason::Timeout`] beats hanging.
const DIG_FINISH_GRACE: Duration = Duration::from_secs(10);

/// `diggingTask` rejection message, identical to upstream's.
const DIG_ABORTED: &str = "Digging aborted";

/// Upstream's own bound: `placeBlock` uses `onceWithCleanup(timeout: 5000)`
/// and `placeEntity` a `setTimeout(5000)` before rejecting.
const PLACE_TIMEOUT: Duration = Duration::from_secs(5);

/// `bot.dig`'s `forceLook` argument (`lib/plugins/digging.js`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ForceLook {
    /// Default — look at the block and wait one tick for the look to land.
    #[default]
    Look,
    /// `true` — look and don't yield before starting the dig.
    Force,
    /// `'ignore'` — skip looking entirely.
    Ignore,
}

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

/// `options.half` for the generic-place cursor (`lib/plugins/generic_place.js`)
/// — `'top'`/`'bottom'` nudge the reported click 0.25 up/down on a side face
/// (slab/stair halves).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceHalf {
    /// `'top'` — `dy += 0.25`.
    Top,
    /// `'bottom'` — `dy -= 0.25`.
    Bottom,
}

/// `bot._genericPlace`'s `options` (`lib/plugins/generic_place.js`), the
/// [`Bot::place_block_opts`]/[`Bot::place_entity_opts`] argument.
/// Upstream's public `placeBlock`/`placeEntity` don't expose it; the
/// internal `_genericPlace`/`_*WithOptions` calls do — the `_opts`
/// variants are the same surface.
#[derive(Clone, Debug, Default)]
pub struct PlaceOpts {
    /// `options.offhand` — place with the off-hand item (`hand: 1` on the
    /// wire), erroring `must be holding an item in the off-hand to place`
    /// when slot 45 is empty. Otherwise the main hand must hold an item.
    pub offhand: bool,
    /// `options.half` — on a side face (`dy == 0.5`) shifts the cursor
    /// 0.25 toward the named half.
    pub half: Option<PlaceHalf>,
    /// `options.delta` — replaces the computed cursor point outright.
    pub delta: Option<Vec3>,
    /// `options.forceLook` — `Ignore` skips the pre-place look; `Look`
    /// looks with `force: false`, `Force` with `true` (upstream passes
    /// the value straight to `lookAt`).
    pub force_look: ForceLook,
    /// `options.swingArm` — the hand to swing after the packet; `None`
    /// is upstream's falsy and sends no swing.
    pub swing_arm: Option<InteractionHand>,
    /// `options.showHand` — accepted for signature parity; it has no
    /// wire meaning in this protocol (the `bot.swingArm` row in
    /// api-parity.md). Spawn-egg places set it `false` upstream.
    pub show_hand: bool,
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

/// What a held item will spawn when placed with [`Bot::place_entity`]
/// (`lib/plugins/place_entity.js`'s `expectedName` reduction).
#[derive(Debug)]
struct PlacedEntity {
    /// Registry name the placed entity is expected to have (`boat` covers
    /// every `*_boat`/`*_chest_boat` variant).
    expected: String,
    /// Upstream's `maxDistance` — bat 4, boat 3, everything else 2.
    max_distance: f64,
    /// Boats also send a bare `use_item` packet (`useItemWithOwnPacket`).
    is_boat: bool,
}

impl PlacedEntity {
    fn from_item(item_name: &str) -> Option<Self> {
        let (expected, is_boat) = if item_name == "end_crystal" || item_name == "armor_stand" {
            (item_name.to_string(), false)
        } else if item_name.ends_with("_boat") {
            ("boat".to_string(), true)
        } else {
            (item_name.strip_suffix("_spawn_egg")?.to_string(), false)
        };
        let max_distance = match expected.as_str() {
            "bat" => 4.,
            "boat" => 3.,
            _ => 2.,
        };
        Some(Self {
            expected,
            max_distance,
            is_boat,
        })
    }

    fn matches(&self, kind_name: &str) -> bool {
        kind_name == self.expected || (self.is_boat && kind_name.ends_with("_boat"))
    }
}

/// Dig bookkeeping kept on the client entity.
///
/// azalea's `Mining`/`MiningQueued` components already carry the target
/// block and face; this adds the two things upstream tracks beside them:
/// which `dig` call `stop_digging` should abort (`dig_seq`/`aborted_seq`)
/// and `bot.lastDigTime`.
#[derive(Component, Default)]
pub(crate) struct InteractShared {
    /// Bumped every time `dig` starts — the current dig generation.
    dig_seq: u64,
    /// `dig_seq` value at the last `stop_digging`; digs with `seq <=` this
    /// have been aborted.
    aborted_seq: u64,
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

/// Upstream `vectorToDirection` (place_block.js / inventory.js
/// `activateBlock`): y→z→x precedence, `None` when nothing is non-zero.
fn face_direction(v: Vec3) -> Option<Direction> {
    Some(if v.y < 0. {
        Direction::Down
    } else if v.y > 0. {
        Direction::Up
    } else if v.z < 0. {
        Direction::North
    } else if v.z > 0. {
        Direction::South
    } else if v.x < 0. {
        Direction::West
    } else if v.x > 0. {
        Direction::East
    } else {
        return None;
    })
}

/// `dest`, face, and click-point for a place/activate: `reference + face`,
/// `vectorToDirection(face)`, `0.5 + face·0.5` (`_genericPlace`'s
/// `dx/dy/dz` and `activateBlock`'s default `cursorPos`).
fn place_geometry(reference: BlockPos, face_vector: Vec3) -> Option<(BlockPos, Direction, Vec3)> {
    let direction = face_direction(face_vector)?;
    let dest = reference
        + BlockPos {
            x: face_vector.x.round() as i32,
            y: face_vector.y.round() as i32,
            z: face_vector.z.round() as i32,
        };
    let cursor = Vec3 {
        x: 0.5 + face_vector.x * 0.5,
        y: 0.5 + face_vector.y * 0.5,
        z: 0.5 + face_vector.z * 0.5,
    };
    Some((dest, direction, cursor))
}

/// `_genericPlace`'s cursor resolution: `0.5 + face·0.5`, `half` nudging
/// `dy` 0.25 on side faces (`dy == 0.5`), `delta` replacing the result
/// outright.
fn place_cursor(face_vector: Vec3, opts: &PlaceOpts) -> Vec3 {
    let mut cursor = Vec3 {
        x: 0.5 + face_vector.x * 0.5,
        y: 0.5 + face_vector.y * 0.5,
        z: 0.5 + face_vector.z * 0.5,
    };
    if cursor.y == 0.5 {
        match opts.half {
            Some(PlaceHalf::Top) => cursor.y += 0.25,
            Some(PlaceHalf::Bottom) => cursor.y -= 0.25,
            None => {}
        }
    }
    if let Some(delta) = opts.delta {
        cursor = delta;
    }
    cursor
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
    /// `Error::NotConnected` before the bot has spawned; `Error::Operation`
    /// for an unbreakable/undiggable target, an unreachable face, or a
    /// stopped/interrupted dig; `Error::Timeout` when the block never
    /// breaks (upstream would wait forever).
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
            return Err(Error::Operation(format!(
                "dig time for {} is Infinity",
                block.name()
            )));
        };
        // azalea `unwrap`s HitResultComponent when handling the start
        // event — missing means the pick systems haven't initialized yet,
        // not that we're offline
        if self.client.get_component::<HitResultComponent>().is_none() {
            return Err(Error::Operation("bot isn't fully spawned yet".to_string()));
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
        let my_seq = {
            let mut ecs = self.client.ecs.write();
            let Some(mut shared) = ecs.get_mut::<InteractShared>(self.client.entity) else {
                return Err(Error::NotConnected);
            };
            shared.dig_seq += 1;
            shared.dig_seq
        };
        self.client.start_mining(pos);

        let outcome = self
            .dig_wait(
                pos,
                my_seq,
                tokio::time::Instant::now() + wait + DIG_FINISH_GRACE,
            )
            .await;

        // upstream's `lastDigTime = Date.now()` runs on resolve or abort
        {
            let mut ecs = self.client.ecs.write();
            if let Some(mut shared) = ecs.get_mut::<InteractShared>(self.client.entity) {
                shared.last_dig_time = Some(Instant::now());
            }
        }
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
                Err(Error::Operation(DIG_ABORTED.to_string()))
            }
            DigExit::Disconnected => Err(Error::NotConnected),
        }
    }

    /// The dig wait after `start_mining` is on the wire: poll each tick
    /// until the block turns air, the dig aborts, the deadline hits, or
    /// the connection ends. Split from `dig_opts` so tests can drive it —
    /// the `HitResultComponent` it gates on can't be constructed outside
    /// azalea.
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
    /// Stamps the dig generation even when no `Mining`/`MiningQueued`
    /// exists yet (a stop in the `start_mining` window still aborts the
    /// pending `dig`), then sends azalea's `StopMiningBlockEvent` (→
    /// `AbortDestroyBlock` packet) and drops a stale `MiningQueued`.
    pub fn stop_digging(&self) {
        let entity = self.client.entity;
        let mut ecs = self.client.ecs.write();
        let mining = ecs.get::<Mining>(entity).is_some();
        let queued = ecs.get::<MiningQueued>(entity).is_some();
        // stamp before the early return: a stop landing in the
        // `start_mining`→`MiningQueued` window must not be swallowed
        if let Some(mut shared) = ecs.get_mut::<InteractShared>(entity) {
            shared.abort_current();
        }
        if !mining && !queued {
            return;
        }
        if queued {
            ecs.entity_mut(entity).remove::<MiningQueued>();
        }
        if mining {
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

    /// `bot.targetDigBlock` — the block currently being dug, if any.
    pub fn target_dig_block(&self) -> Option<Block> {
        let (pos, _) = self.dig_target()?;
        self.block_at(pos)
    }

    /// `bot.targetDigFace` — the face the dig is pointed at (`0..=5`,
    /// same ordinals as upstream's `block.face`).
    pub fn target_dig_face(&self) -> Option<Direction> {
        self.dig_target().map(|(_, dir)| dir)
    }

    /// `bot.lastDigTime` — when the last dig finished or aborted.
    pub fn last_dig_time(&self) -> Option<Instant> {
        self.client
            .get_component::<InteractShared>()
            .and_then(|s| s.last_dig_time)
    }

    /// `bot.placeBlock(referenceBlock, faceVector)` — click `faceVector`'s
    /// face of `referenceBlock` with the held item and wait for the server
    /// to confirm `reference + faceVector` changed (upstream waits on
    /// `blockUpdate:{dest}`; ours has a 5s cap).
    ///
    /// Returns the new block and emits `blockPlaced`. Like upstream, a
    /// `blockUpdate` on the *reference* block acts as the server ack that
    /// gates acceptance of the destination update.
    ///
    /// # Errors
    ///
    /// `Error::Operation` for a zero face vector, an empty hand, a missing
    /// server ack, or the destination staying unchanged ("refused");
    /// `Error::Timeout` when the ack arrived but the update didn't.
    pub async fn place_block(&self, reference_block: &Block, face_vector: Vec3) -> Result<Block> {
        // upstream's public `placeBlock` is `_placeBlockWithOptions`
        // called with `{ swingArm: 'right' }` — the default options.
        self.place_block_opts(
            reference_block,
            face_vector,
            &PlaceOpts {
                swing_arm: Some(InteractionHand::MainHand),
                ..Default::default()
            },
        )
        .await
    }

    /// `bot._placeBlockWithOptions(referenceBlock, faceVector, options)` —
    /// [`Bot::place_block`] with the full `_genericPlace` options surface
    /// (offhand, cursor `half`/`delta`, `forceLook`, `swingArm`). An
    /// offhand place still names the main-hand item in the refusal error,
    /// like upstream's `bot.heldItem?.name`.
    pub async fn place_block_opts(
        &self,
        reference_block: &Block,
        face_vector: Vec3,
        opts: &PlaceOpts,
    ) -> Result<Block> {
        let (dest, ..) = place_geometry(reference_block.pos, face_vector)
            .ok_or_else(|| Error::Operation(format!("invalid direction vector {face_vector:?}")))?;
        let old_block = self.block_at(dest);
        let old_kind = old_block.as_ref().map(|b| b.kind());
        let held_name = self
            .held_item()
            .map(|h| h.name().to_string())
            .unwrap_or_else(|| "block".to_string());
        let mut rx = self.events();
        self.generic_place(reference_block.pos, face_vector, opts)
            .await?;
        let new_block = self
            .wait_place_result(reference_block.pos, dest, old_kind, &held_name, &mut rx)
            .await?;
        self.emit_event(BotEvent::BlockPlaced {
            old: old_block,
            new: new_block.clone(),
        });
        Ok(new_block)
    }

    /// `bot.placeEntity(referenceBlock, faceVector)` — place an entity
    /// (spawn egg, armor stand, end crystal, boat) with the held item and
    /// wait ≤5s for the matching spawn near `reference + faceVector`.
    /// Emits `entityPlaced` (undocumented upstream) and resolves the entity.
    ///
    /// # Errors
    ///
    /// `Error::Operation` when the held item can't spawn an entity or no
    /// matching entity appears in time.
    pub async fn place_entity(
        &self,
        reference_block: &Block,
        face_vector: Vec3,
    ) -> Result<azalea::EntityRef> {
        self.place_entity_opts(reference_block, face_vector, &PlaceOpts::default())
            .await
    }

    /// `bot._placeEntityWithOptions(referenceBlock, faceVector, options)` —
    /// [`Bot::place_entity`] with the `_genericPlace` options surface.
    /// Like upstream, `bot.heldItem` is still required even for
    /// `offhand: true` (it names the expected entity), and an unset
    /// `swingArm` defaults to the placing hand.
    pub async fn place_entity_opts(
        &self,
        reference_block: &Block,
        face_vector: Vec3,
        opts: &PlaceOpts,
    ) -> Result<azalea::EntityRef> {
        let held = self.held_item().ok_or_else(|| {
            Error::Operation("must be holding an item to place an entity".to_string())
        })?;
        let item_name = held.kind().to_str();
        let Some(expected) = PlacedEntity::from_item(item_name) else {
            return Err(Error::Operation(format!(
                "Cannot place entity using {item_name}"
            )));
        };
        let (dest, ..) = place_geometry(reference_block.pos, face_vector)
            .ok_or_else(|| Error::Operation(format!("invalid direction vector {face_vector:?}")))?;
        let dest_corner = Vec3 {
            x: dest.x as f64,
            y: dest.y as f64,
            z: dest.z as f64,
        };
        // upstream: `if (!options.swingArm) options.swingArm =
        // options.offhand ? 'left' : 'right'` — swing the placing hand.
        let mut opts = opts.clone();
        if opts.swing_arm.is_none() {
            opts.swing_arm = Some(if opts.offhand {
                InteractionHand::OffHand
            } else {
                InteractionHand::MainHand
            });
        }
        let mut rx = self.events();
        let hand = self
            .generic_place(reference_block.pos, face_vector, &opts)
            .await?;
        if expected.is_boat {
            // upstream's `useItemWithOwnPacket` — a bare use-item on ≥1.19
            // sent for the hand the place packet carried
            let seq = self.next_sequence();
            let (y_rot, x_rot) = self
                .client
                .get_component::<azalea::entity::LookDirection>()
                .map(|l| (l.y_rot(), l.x_rot()))
                .unwrap_or_default();
            self.client.write_packet(ServerboundUseItem {
                hand,
                seq,
                y_rot,
                x_rot,
            });
        }
        let entity = tokio::time::timeout(PLACE_TIMEOUT, async {
            loop {
                match rx.recv().await {
                    Ok(BotEvent::EntitySpawn(entity)) => {
                        let kind = entity
                            .get_component::<azalea::entity::EntityKindComponent>()
                            .map(|k| k.0.to_str());
                        let pos = entity.get_component::<Position>().map(|p| **p);
                        if kind.is_some_and(|k| expected.matches(k))
                            && pos
                                .is_some_and(|p| p.distance_to(dest_corner) < expected.max_distance)
                        {
                            return Ok(entity);
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::NotConnected);
                    }
                }
            }
        })
        .await
        .map_err(|_| Error::Operation("Failed to place entity".to_string()))??;
        self.emit_event(BotEvent::EntityPlaced(entity.clone()));
        Ok(entity)
    }

    /// `bot.activateBlock(block, direction, cursorPos)` — right-click a
    /// block: look at the clicked point, send `use_item_on`, swing.
    /// Defaults match upstream: `direction = Vec3(0,1,0)` (top face) and
    /// `cursorPos = 0.5 + direction·0.5` (the face center).
    ///
    /// The click point lands on the wire (`BlockHit.location`), so
    /// non-default `cursor_pos` values are honored — a difference from an
    /// azalea `block_interact` call, which raycasts the target itself.
    pub async fn activate_block(
        &self,
        block: &Block,
        direction: Option<Vec3>,
        cursor_pos: Option<Vec3>,
    ) -> Result<()> {
        let dir_vec = direction.unwrap_or(Vec3::new(0., 1., 0.));
        let Some(face) = face_direction(dir_vec) else {
            return Err(Error::Operation(format!(
                "invalid direction vector {dir_vec:?}"
            )));
        };
        let cursor = cursor_pos.unwrap_or(Vec3 {
            x: 0.5 + dir_vec.x * 0.5,
            y: 0.5 + dir_vec.y * 0.5,
            z: 0.5 + dir_vec.z * 0.5,
        });
        // upstream activateBlock: `hand: 0`, always look, always swing —
        // it predates `_genericPlace` and takes no options.
        self.use_item_on(
            block.pos,
            face,
            cursor,
            InteractionHand::MainHand,
            ForceLook::Look,
            Some(InteractionHand::MainHand),
        )
        .await
    }

    /// The `Mining`/`MiningQueued` target, whichever is active — azalea
    /// derives the face from `HitResultComponent` when the dig starts.
    fn dig_target(&self) -> Option<(BlockPos, Direction)> {
        let ecs = self.client.ecs.read();
        let entity = self.client.entity;
        ecs.get::<Mining>(entity)
            .map(|m| (m.pos, m.dir))
            .or_else(|| {
                ecs.get::<MiningQueued>(entity)
                    .map(|q| (q.position, q.direction))
            })
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
        Err(Error::Operation("Block not in view".to_string()))
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

    /// Increment the action sequence like azalea's predicted interactions
    /// (`BlockStatePredictionHandler::start_predicting`); `0` pre-spawn.
    fn next_sequence(&self) -> u32 {
        self.client
            .ecs
            .write()
            .get_mut::<BlockStatePredictionHandler>(self.client.entity)
            .map(|mut h| h.start_predicting())
            .unwrap_or(0)
    }

    /// The `use_item_on` wire send shared by [`Bot::generic_place`] and
    /// [`Bot::activate_block`]: look at the clicked point (unless the
    /// caller opted out), send `ServerboundUseItemOn` for
    /// `pos`/`direction`/`cursor`/`hand`, then swing if asked — the
    /// ordering upstream's "The swing must follow use_item_on" comment
    /// enforces.
    async fn use_item_on(
        &self,
        pos: BlockPos,
        direction: Direction,
        cursor: Vec3,
        hand: InteractionHand,
        force_look: ForceLook,
        swing_arm: Option<InteractionHand>,
    ) -> Result<()> {
        let location = Vec3 {
            x: pos.x as f64 + cursor.x,
            y: pos.y as f64 + cursor.y,
            z: pos.z as f64 + cursor.z,
        };
        if force_look != ForceLook::Ignore {
            self.look_at(location, force_look == ForceLook::Force)
                .await?;
        }
        let seq = self.next_sequence();
        self.client.write_packet(ServerboundUseItemOn {
            hand,
            block_hit: BlockHit {
                block_pos: pos,
                direction,
                location,
                inside: false,
                world_border: false,
            },
            seq,
        });
        if let Some(arm) = swing_arm {
            self.swing_arm(arm);
        }
        Ok(())
    }

    /// `bot._genericPlace` (`lib/plugins/generic_place.js`) — the
    /// options-aware place path behind `place_block_opts`/
    /// `place_entity_opts`. Checks the chosen hand holds an item,
    /// resolves the cursor (`half` nudge, `delta` override), and returns
    /// the hand the packet carried (`place_entity`'s boat `use_item`
    /// needs it).
    async fn generic_place(
        &self,
        reference: BlockPos,
        face_vector: Vec3,
        opts: &PlaceOpts,
    ) -> Result<InteractionHand> {
        let direction = face_direction(face_vector)
            .ok_or_else(|| Error::Operation(format!("invalid direction vector {face_vector:?}")))?;
        // upstream checks the hand before computing the cursor: offhand
        // wants `bot.inventory.slots[45]`, main hand wants `bot.heldItem`.
        let hand = if opts.offhand {
            if !self
                .inventory()
                .slot(45)
                .is_some_and(|i| i.stack.is_present())
            {
                return Err(Error::Operation(
                    "must be holding an item in the off-hand to place".to_string(),
                ));
            }
            InteractionHand::OffHand
        } else {
            if self.held_item().is_none() {
                return Err(Error::Operation(
                    "must be holding an item to place".to_string(),
                ));
            }
            InteractionHand::MainHand
        };
        let cursor = place_cursor(face_vector, opts);
        self.use_item_on(
            reference,
            direction,
            cursor,
            hand,
            opts.force_look,
            opts.swing_arm,
        )
        .await?;
        Ok(hand)
    }

    /// Upstream `placeBlock`'s wait: a `blockUpdate` on the reference block
    /// is the server ack; a dest update resolves once it's newer than the
    /// ack (or the previous state was unobserved) — and rejects if the
    /// dest is still what it was before the place.
    async fn wait_place_result(
        &self,
        reference: BlockPos,
        dest: BlockPos,
        old_kind: Option<azalea::registry::builtin::BlockKind>,
        held_name: &str,
        rx: &mut broadcast::Receiver<BotEvent>,
    ) -> Result<Block> {
        let mut acked = false;
        tokio::time::timeout(PLACE_TIMEOUT, async {
            loop {
                match rx.recv().await {
                    Ok(BotEvent::BlockUpdate { new, .. }) if new.pos == reference => {
                        acked = true;
                    }
                    Ok(BotEvent::BlockUpdate { old, new }) if new.pos == dest => {
                        if old.is_none() || old.map(|o| o.kind()) != Some(new.kind()) || acked {
                            if Some(new.kind()) == old_kind {
                                return Err(Error::Operation(format!(
                                    "Server refused to place {held_name} at {dest}: \
                                     the block is still {}",
                                    new.name()
                                )));
                            }
                            return Ok(new);
                        }
                        // dest update older than the ack — keep waiting
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(Error::NotConnected);
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            if acked {
                Error::Timeout
            } else {
                Error::Operation(format!("Server did not answer the placement at {dest}"))
            }
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_direction_precedence() {
        // y wins over x/z like upstream's vectorToDirection
        assert_eq!(face_direction(Vec3::new(1., 1., 0.)), Some(Direction::Up));
        assert_eq!(
            face_direction(Vec3::new(0., -1., 0.)),
            Some(Direction::Down)
        );
        assert_eq!(
            face_direction(Vec3::new(0., 0., -1.)),
            Some(Direction::North)
        );
        assert_eq!(
            face_direction(Vec3::new(0., 0., 1.)),
            Some(Direction::South)
        );
        assert_eq!(
            face_direction(Vec3::new(-1., 0., 0.)),
            Some(Direction::West)
        );
        assert_eq!(face_direction(Vec3::new(1., 0., 0.)), Some(Direction::East));
        assert_eq!(face_direction(Vec3::new(0., 0., 0.)), None);
    }

    /// `_genericPlace`'s cursor math: `0.5 + face·0.5` default, `half`
    /// nudging `dy` only on side faces (upstream's slab/stair steering),
    /// `delta` replacing the result outright.
    #[test]
    fn generic_place_cursor_resolution() {
        let side = Vec3::new(1., 0., 0.);
        let top = Vec3::new(0., 1., 0.);

        // defaults — the face center.
        assert_eq!(
            place_cursor(side, &PlaceOpts::default()),
            Vec3::new(1., 0.5, 0.5)
        );
        assert_eq!(
            place_cursor(top, &PlaceOpts::default()),
            Vec3::new(0.5, 1., 0.5)
        );

        // `half` applies only when `dy == 0.5` — a top face's `dy` is
        // already 1.0, so it passes through unchanged.
        let top_half = PlaceOpts {
            half: Some(PlaceHalf::Top),
            ..Default::default()
        };
        assert_eq!(place_cursor(side, &top_half), Vec3::new(1., 0.75, 0.5));
        assert_eq!(place_cursor(top, &top_half), Vec3::new(0.5, 1., 0.5));
        let bottom_half = PlaceOpts {
            half: Some(PlaceHalf::Bottom),
            ..Default::default()
        };
        assert_eq!(place_cursor(side, &bottom_half), Vec3::new(1., 0.25, 0.5));

        // `delta` wins over everything, including a nudged `half`.
        let with_delta = PlaceOpts {
            half: Some(PlaceHalf::Top),
            delta: Some(Vec3::new(0.1, 0.2, 0.3)),
            ..Default::default()
        };
        assert_eq!(place_cursor(side, &with_delta), Vec3::new(0.1, 0.2, 0.3));
    }

    /// The hand gate mirrors `_genericPlace`: `offhand` requires slot 45,
    /// the main-hand path requires `heldItem` — each with upstream's
    /// message.
    #[tokio::test]
    async fn generic_place_requires_item_in_chosen_hand() {
        use azalea::entity::inventory::Inventory;

        let client = crate::test_util::live_entity_client(Inventory::default());
        let bot = crate::Bot::new(client);
        let face = Vec3::new(0., 1., 0.);
        let pos = BlockPos::new(0, 64, 0);

        let err = bot
            .generic_place(
                pos,
                face,
                &PlaceOpts {
                    offhand: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be holding an item in the off-hand to place"),
            "unexpected error: {err}"
        );

        let err = bot
            .generic_place(pos, face, &PlaceOpts::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must be holding an item to place"),
            "unexpected error: {err}"
        );
    }

    /// `place_entity`'s swing default: unset `swingArm` follows the
    /// placing hand (`'left'` for offhand, `'right'` otherwise) — the
    /// same conditional upstream applies before `_genericPlace`.
    #[test]
    fn place_entity_swing_defaults_to_placing_hand() {
        // the default-fill logic lives inline in place_entity_opts, but
        // its contract is simple enough to pin down here:
        let resolve = |opts: &PlaceOpts| {
            opts.swing_arm.unwrap_or(if opts.offhand {
                InteractionHand::OffHand
            } else {
                InteractionHand::MainHand
            })
        };
        assert_eq!(resolve(&PlaceOpts::default()), InteractionHand::MainHand);
        assert_eq!(
            resolve(&PlaceOpts {
                offhand: true,
                ..Default::default()
            }),
            InteractionHand::OffHand
        );
        assert_eq!(
            resolve(&PlaceOpts {
                swing_arm: Some(InteractionHand::OffHand),
                ..Default::default()
            }),
            InteractionHand::OffHand
        );
    }

    #[test]
    fn place_geometry_top_and_side() {
        let reference = BlockPos::new(4, 64, 4);
        let (dest, dir, cursor) =
            place_geometry(reference, Vec3::new(0., 1., 0.)).expect("valid face");
        assert_eq!(dest, BlockPos::new(4, 65, 4));
        assert_eq!(dir, Direction::Up);
        assert_eq!(cursor, Vec3::new(0.5, 1., 0.5));

        let (dest, dir, cursor) =
            place_geometry(reference, Vec3::new(-1., 0., 0.)).expect("valid face");
        assert_eq!(dest, BlockPos::new(3, 64, 4));
        assert_eq!(dir, Direction::West);
        assert_eq!(cursor, Vec3::new(0., 0.5, 0.5));

        assert_eq!(place_geometry(reference, Vec3::new(0., 0., 0.)), None);
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

    #[test]
    fn placed_entity_name_mapping() {
        let crystal = PlacedEntity::from_item("end_crystal").expect("crystal");
        assert_eq!(crystal.expected, "end_crystal");
        assert_eq!(crystal.max_distance, 2.);
        assert!(!crystal.is_boat);

        let boat = PlacedEntity::from_item("oak_boat").expect("boat");
        assert_eq!(boat.expected, "boat");
        assert_eq!(boat.max_distance, 3.);
        assert!(boat.is_boat);
        assert!(boat.matches("acacia_boat"));
        assert!(boat.matches("oak_chest_boat"));
        assert!(!boat.matches("armor_stand"));

        let cow = PlacedEntity::from_item("cow_spawn_egg").expect("egg");
        assert_eq!(cow.expected, "cow");
        assert_eq!(cow.max_distance, 2.);
        assert!(!cow.is_boat);

        let bat = PlacedEntity::from_item("bat_spawn_egg").expect("bat");
        assert_eq!(bat.max_distance, 4.);

        assert!(PlacedEntity::from_item("stick").is_none());
    }
}
