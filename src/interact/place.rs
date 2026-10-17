//! Block/entity placement and activation: `place_block`, `place_entity`,
//! `activate_block`, and the geometry/cursor helpers they share. Split out
//! of `interact` alongside the dig machinery.

use std::time::Duration;

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use mineflayer_engine::{
    BlockPos, Vec3,
    core::direction::Direction,
    entity::Position,
    interact::BlockStatePredictionHandler,
    protocol::packets::game::{
        s_interact::InteractionHand,
        s_use_item::ServerboundUseItem,
        s_use_item_on::{BlockHit, ServerboundUseItemOn},
    },
};
use tokio::sync::broadcast;

use super::ForceLook;
use crate::{Bot, BotEvent, Error, Result, world::Block};

/// Upstream's own bound: `placeBlock` uses `onceWithCleanup(timeout: 5000)`
/// and `placeEntity` a `setTimeout(5000)` before rejecting.
const PLACE_TIMEOUT: Duration = Duration::from_secs(5);

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
#[derive(Clone, Debug)]
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
    /// `options.showHand` — `false` makes the swing packet carry the
    /// wire default `hand: 0` (main) regardless of `swing_arm`, like
    /// upstream `swingArm(arm, false)` omitting the `hand` field.
    /// `place_entity_opts` sets it for spawn eggs like upstream.
    pub show_hand: bool,
}

impl Default for PlaceOpts {
    /// Upstream `swingArm(arm, showHand = true)` — an unset option shows
    /// the hand, so `Default` can't be derived (`bool` would give the
    /// suppressing value).
    fn default() -> Self {
        Self {
            offhand: false,
            half: None,
            delta: None,
            force_look: ForceLook::default(),
            swing_arm: None,
            show_hand: true,
        }
    }
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

/// The `swingArm`/`showHand` pair upstream passes to `bot.swingArm`
/// after the `use_item_on` packet — kept together because `show_hand:
/// false` changes which hand the packet reports (wire default `0` =
/// main), not whether it swings.
#[derive(Clone, Copy)]
struct SwingArm {
    /// The hand to animate (`swingArm`).
    hand: InteractionHand,
    /// `showHand` — `false` sends `hand: 0` like upstream's omitted
    /// `hand` field.
    show: bool,
}

impl Bot {
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
    /// `Error::InvalidArgument` for a zero face vector, `Error::InvalidState`
    /// for an empty hand, `Error::Failed` for a missing server ack or the
    /// destination staying unchanged ("refused");
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
        let (dest, ..) = place_geometry(reference_block.pos, face_vector).ok_or_else(|| {
            Error::InvalidArgument(format!("invalid direction vector {face_vector:?}"))
        })?;
        let old_block = self.block_at(dest);
        let old_kind = old_block.as_ref().map(|b| b.kind());
        let mut rx = self.subscribe();
        self.generic_place(reference_block.pos, face_vector, opts)
            .await?;
        let new_block = self
            .wait_place_result(reference_block.pos, dest, old_kind, &mut rx)
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
    /// `Error::InvalidState` for an empty hand, `Error::Unsupported` when the
    /// held item can't spawn an entity, `Error::Failed` when no matching
    /// entity appears in time.
    pub async fn place_entity(
        &self,
        reference_block: &Block,
        face_vector: Vec3,
    ) -> Result<mineflayer_engine::EntityRef> {
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
    ) -> Result<mineflayer_engine::EntityRef> {
        let held = self.held_item().ok_or_else(|| {
            Error::InvalidState("must be holding an item to place an entity".to_string())
        })?;
        // `Item::name()` is un-namespaced like upstream's `heldItem.name`
        // (`ItemKind::to_str()` is `minecraft:<id>` — feeding it here made
        // `end_crystal`/`armor_stand` unplaceable: the literal compare
        // never matched the prefix).
        let item_name = held.name();
        let Some(expected) = PlacedEntity::from_item(item_name) else {
            return Err(Error::Unsupported(format!(
                "Cannot place entity using {item_name}"
            )));
        };
        let (dest, ..) = place_geometry(reference_block.pos, face_vector).ok_or_else(|| {
            Error::InvalidArgument(format!("invalid direction vector {face_vector:?}"))
        })?;
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
        // upstream: `if (type === 'spawn_egg') options.showHand = false`
        // — a spawn-egg place suppresses the hand on the swing packet.
        if item_name.ends_with("_spawn_egg") {
            opts.show_hand = false;
        }
        let mut rx = self.subscribe();
        let hand = self
            .generic_place(reference_block.pos, face_vector, &opts)
            .await?;
        if expected.is_boat {
            // upstream's `useItemWithOwnPacket` — a bare use-item on ≥1.19
            // sent for the hand the place packet carried
            let seq = self.next_sequence();
            let (y_rot, x_rot) = self
                .client
                .get_component::<mineflayer_engine::entity::LookDirection>()
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
                            .get_component::<mineflayer_engine::entity::EntityKindComponent>()
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
        .map_err(|_| Error::Failed("Failed to place entity".to_string()))??;
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
    /// The engine `block_interact` call, which raycasts the target itself.
    pub async fn activate_block(
        &self,
        block: &Block,
        direction: Option<Vec3>,
        cursor_pos: Option<Vec3>,
    ) -> Result<()> {
        let dir_vec = direction.unwrap_or(Vec3::new(0., 1., 0.));
        let Some(face) = face_direction(dir_vec) else {
            return Err(Error::InvalidArgument(format!(
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
            Some(SwingArm {
                hand: InteractionHand::MainHand,
                show: true,
            }),
        )
        .await
    }

    /// Increment the action sequence like the engine's predicted interactions
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
    /// `show_hand` mirrors upstream `swingArm(arm, showHand)`: `false`
    /// omits `hand` from `arm_animation`, which serializes to the wire
    /// default `hand: 0` — a *main-hand* animation regardless of the
    /// requested arm (the spawn-egg case upstream uses it for).
    async fn use_item_on(
        &self,
        pos: BlockPos,
        direction: Direction,
        cursor: Vec3,
        hand: InteractionHand,
        force_look: ForceLook,
        swing_arm: Option<SwingArm>,
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
        if let Some(swing) = swing_arm {
            // `showHand: false` sends the wire default `hand: 0` —
            // upstream's packet literally omits the field.
            self.swing_arm(if swing.show {
                swing.hand
            } else {
                InteractionHand::MainHand
            });
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
        let direction = face_direction(face_vector).ok_or_else(|| {
            Error::InvalidArgument(format!("invalid direction vector {face_vector:?}"))
        })?;
        // upstream checks the hand before computing the cursor: offhand
        // wants `bot.inventory.slots[45]`, main hand wants `bot.heldItem`.
        let hand = if opts.offhand {
            if !self
                .inventory()
                .slot(45)
                .is_some_and(|i| i.stack.is_present())
            {
                return Err(Error::InvalidState(
                    "must be holding an item in the off-hand to place".to_string(),
                ));
            }
            InteractionHand::OffHand
        } else {
            if self.held_item().is_none() {
                return Err(Error::InvalidState(
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
            opts.swing_arm.map(|hand| SwingArm {
                hand,
                show: opts.show_hand,
            }),
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
        old_kind: Option<mineflayer_engine::registry::builtin::BlockKind>,
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
                                // upstream reads `bot.heldItem?.name` at
                                // throw time — a mid-wait switch names
                                // the current item, not the placed one.
                                let held_name = self
                                    .held_item()
                                    .map(|h| h.name().to_string())
                                    .unwrap_or_else(|| "block".to_string());
                                return Err(Error::Failed(format!(
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
                Error::Failed(format!("Server did not answer the placement at {dest}"))
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
        use mineflayer_engine::entity::inventory::Inventory;

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

    /// `place_entity`'s item-name contract: `from_item` compares
    /// upstream's bare `heldItem.name` literals — feeding it
    /// `ItemKind::to_str()` (`"minecraft:<id>"`) made `end_crystal`/
    /// `armor_stand` unplaceable. Pin the `Item::name()` strip and the
    /// four types upstream supports.
    #[test]
    fn placed_entity_uses_unnamespaced_item_name() {
        use mineflayer_engine::{inventory::ItemStack, registry::builtin::ItemKind};

        let item = crate::windows::Item {
            stack: ItemStack::new(ItemKind::EndCrystal, 1),
            slot: 36,
        };
        // `kind().to_str()` is namespaced — the bug — `name()` is the
        // bare literal upstream's `heldItem.name` produces.
        assert_eq!(item.kind().to_str(), "minecraft:end_crystal");
        assert_eq!(item.name(), "end_crystal");

        for (name, expected) in [
            ("end_crystal", "end_crystal"),
            ("armor_stand", "armor_stand"),
            ("oak_boat", "boat"),
            ("zombie_spawn_egg", "zombie"),
        ] {
            assert_eq!(PlacedEntity::from_item(name).unwrap().expected, expected);
        }
        assert!(PlacedEntity::from_item("minecraft:end_crystal").is_none());
        assert!(PlacedEntity::from_item("dirt").is_none());
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
