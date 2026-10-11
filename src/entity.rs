//! Entities, players, and combat — queries, the entity event family, and the
//! attack/interact/mount action surface.
//!
//! Entity *state* lives in azalea's ECS (tracked by its `EntityPlugin`); this
//! module only adds what azalea doesn't expose: packet-driven entity events
//! ([`BotEvent`] variants decoded in [`observe_event`]), tracked side state
//! that isn't in the ECS (equipment, raw attributes, vehicles), and the
//! `bot.*` method surface.

use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, RwLock},
};

use azalea::{
    EntityRef, Event, FormattedText, Vec3,
    core::{difficulty::Difficulty, entity_id::MinecraftEntityId},
    ecs::{entity::Entity, query::With},
    entity::{
        ActiveEffects, EntityDataItem, EntityDataValue, EntityKindComponent, LookDirection,
        MobEffectData, Physics, Pose, Position,
        indexing::EntityIdIndex,
        metadata::{
            AbstractEntityShiftKeyDown, AttachedToTarget, FallFlying, FireworksItem, SleepingPos,
        },
    },
    inventory::{
        ItemStack,
        components::{EquipmentSlot, Fireworks},
    },
    physics::clip::{BlockShapeType, ClipContext, FluidPickType, clip},
    player::{GameProfileComponent, PlayerInfo},
    protocol::packets::game::{
        ClientboundGamePacket,
        c_animate::AnimationAction,
        c_update_attributes::AttributeSnapshot,
        s_attack::ServerboundAttack,
        s_interact::InteractionHand,
        s_player_command::{Action as PlayerCommandAction, ServerboundPlayerCommand},
        s_player_input::ServerboundPlayerInput,
        s_swing::ServerboundSwing,
    },
    registry::builtin::{Attribute, EntityKind, MobEffect},
};
use regex::Regex;

use crate::{Bot, BotEvent, Error, Result};

// ---------------------------------------------------------------------------
// Event bridging (bridge.rs calls `observe_event` for every azalea `Event`)
// ---------------------------------------------------------------------------

/// Entity-domain state the ECS doesn't keep, shared between the bridge task
/// (writer) and `Bot` handles (readers).
///
/// Keyed by [`MinecraftEntityId`] rather than ECS `Entity` so entries stay
/// meaningful for just-removed entities — the ECS side of a removed entity is
/// dropped by azalea before our packet events are even read.
#[derive(Default)]
pub(crate) struct EntityTracker {
    /// Entity ids we've resolved to an ECS `Entity`, kept after despawn so
    /// `entityGone`/`playerCollect` still produce an [`EntityRef`] handle.
    known: HashMap<MinecraftEntityId, Entity>,
    /// Last-seen metadata state per entity, for edge detection on
    /// `entity_metadata` packets (crouch/elytra/sleep).
    meta: HashMap<MinecraftEntityId, EntityMeta>,
    /// `c_set_equipment` contents per entity. azalea drops this packet
    /// (`debug!`-only handler), so equipment lives here.
    equipment: HashMap<MinecraftEntityId, HashMap<EquipmentSlot, ItemStack>>,
    /// `c_update_attributes` contents per entity — azalea only stores the
    /// subset of attributes it uses (no armor), so the raw snapshots are
    /// tracked for [`Bot::get_explosion_damages`].
    attributes: HashMap<MinecraftEntityId, HashMap<Attribute, AttributeSnapshot>>,
    /// Active mob effects per entity (`c_update_mob_effect` payload), so
    /// `entityEffectEnd` can report the effect's data like upstream's
    /// `entity.effects[id]` lookup does.
    effects: HashMap<MinecraftEntityId, HashMap<MobEffect, MobEffectData>>,
    /// `c_set_passengers` passenger lists per vehicle.
    passengers_of: HashMap<MinecraftEntityId, Vec<MinecraftEntityId>>,
    /// Passenger → vehicle link, fed by `c_set_passengers` and
    /// `c_set_entity_link` (leashes).
    vehicle_of: HashMap<MinecraftEntityId, MinecraftEntityId>,
    /// Firework entity ids already counted for `usedFirework`/`fireworkRocketDuration`.
    seen_fireworks: HashSet<MinecraftEntityId>,
    firework_rocket_duration: u32,
    /// `c_change_difficulty` — needed for the explosion formula's player
    /// scaling. `None` until the server sends one (upstream's
    /// `bot.game.difficulty` is likewise only set after the game packet).
    difficulty: Option<Difficulty>,
}

/// Last-seen metadata snapshot for edge detection (sleep isn't tracked —
/// `entitySleep` fires on every packet carrying the flag).
#[derive(Default)]
struct EntityMeta {
    shift_key_down: bool,
    fall_flying: bool,
}

/// Feed one azalea [`Event`] into the entity domain, returning the
/// [`BotEvent`]s it produced. Called by the bridge for *every* event, before
/// `translate` runs, so the returned events are emitted right after the
/// corresponding raw `BotEvent::Packet`/`Spawn`.
pub(crate) fn observe_event(
    tracker: &RwLock<EntityTracker>,
    client: &azalea::Client,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        Event::Packet(packet) => tracker
            .write()
            .unwrap()
            .handle_packet(client, packet.as_ref()),
        // `bot.on('spawn', () => bot.emit('entitySpawn', bot.entity))` upstream
        Event::Spawn => vec![BotEvent::EntitySpawn(client.entity())],
        // upstream resets `bot.entities` on login
        Event::Login => {
            tracker.write().unwrap().clear();
            vec![]
        }
        _ => vec![],
    }
}

/// [`Client::entity_id_by_minecraft_id`](azalea::Client::entity_id_by_minecraft_id)
/// without the panic — azalea resolves the id by `query_self`-ing our own
/// entity for `EntityIdIndex`, but teardown packets race the despawn of
/// that entity.
pub(crate) fn entity_id_by_minecraft_id(
    client: &azalea::Client,
    id: MinecraftEntityId,
) -> Option<Entity> {
    client
        .try_query_self::<&EntityIdIndex, _>(|index| index.get_by_minecraft_entity(id))
        .ok()
        .flatten()
}

/// [`Client::entity_by_minecraft_id`](azalea::Client::entity_by_minecraft_id)
/// without the panic — same despawn race as above.
pub(crate) fn entity_by_minecraft_id(
    client: &azalea::Client,
    id: MinecraftEntityId,
) -> Option<EntityRef> {
    entity_id_by_minecraft_id(client, id).map(|e| client.entity_ref_for(e))
}

impl EntityTracker {
    fn clear(&mut self) {
        *self = EntityTracker::default();
    }

    /// Resolve a protocol entity id to an [`EntityRef`], preferring the live
    /// ECS index and falling back to ids remembered before despawn.
    fn resolve(&mut self, client: &azalea::Client, id: MinecraftEntityId) -> Option<EntityRef> {
        if let Some(entity) = entity_id_by_minecraft_id(client, id) {
            self.known.insert(id, entity);
            return Some(client.entity_ref_for(entity));
        }
        self.known
            .get(&id)
            .map(|&entity| client.entity_ref_for(entity))
    }

    /// Forget everything about a removed entity. If it was our vehicle,
    /// returns the vehicle's id so the caller can emit `Dismounted`
    /// (upstream emits `dismount` when the vehicle goes away).
    fn forget(
        &mut self,
        self_id: Option<MinecraftEntityId>,
        id: MinecraftEntityId,
    ) -> Option<MinecraftEntityId> {
        self.known.remove(&id);
        self.meta.remove(&id);
        self.equipment.remove(&id);
        self.attributes.remove(&id);
        self.effects.remove(&id);
        // the removed entity may itself be a vehicle or a passenger
        self.vehicle_of.remove(&id);
        for passengers in self.passengers_of.values_mut() {
            passengers.retain(|p| *p != id);
        }
        self.passengers_of.remove(&id);
        let mut own_vehicle_gone = None;
        self.vehicle_of.retain(|passenger, vehicle| {
            if *vehicle == id {
                if Some(*passenger) == self_id {
                    own_vehicle_gone = Some(id);
                }
                false
            } else {
                true
            }
        });
        own_vehicle_gone
    }

    /// Decode one clientbound packet into entity-domain [`BotEvent`]s. Runs
    /// after azalea's own packet handler, so the ECS already reflects the
    /// packet (indexes are populated for spawns, cleared for removals).
    fn handle_packet(
        &mut self,
        client: &azalea::Client,
        packet: &ClientboundGamePacket,
    ) -> Vec<BotEvent> {
        use ClientboundGamePacket::*;
        let self_id = client.get_component::<MinecraftEntityId>().map(|id| *id);
        match packet {
            AddEntity(p) => self
                .resolve(client, p.id)
                .map(BotEvent::EntitySpawn)
                .into_iter()
                .collect(),
            RemoveEntities(p) => {
                let mut out = Vec::new();
                for &id in &p.entity_ids {
                    // Resolve before `forget` clears our record — EntityGone
                    // and (if it was our vehicle) Dismounted both carry the
                    // stale handle, like upstream's dead entity object.
                    let entity = self.resolve(client, id);
                    let own_vehicle_gone = self.forget(self_id, id);
                    out.push(BotEvent::EntityGone {
                        entity: entity.clone(),
                        id,
                    });
                    if own_vehicle_gone.is_some() {
                        out.push(BotEvent::Dismounted { vehicle: entity });
                    }
                }
                out
            }
            MoveEntityPos(p) => self.moved(client, p.entity_id),
            MoveEntityPosRot(p) => self.moved(client, p.entity_id),
            MoveEntityRot(p) => self.moved(client, p.entity_id),
            TeleportEntity(p) => self.moved(client, p.id),
            EntityPositionSync(p) => self.moved(client, p.id),
            RotateHead(p) => self.moved(client, p.entity_id),
            SetEntityData(p) => self.metadata_update(client, p.id, &p.packed_items.0, self_id),
            Animate(p) => {
                let Some(entity) = self.resolve(client, p.id) else {
                    return vec![];
                };
                match p.action {
                    AnimationAction::SwingMainHand => vec![BotEvent::EntitySwingArm(entity)],
                    AnimationAction::Hurt => vec![BotEvent::EntityHurt {
                        entity,
                        source: None,
                    }],
                    AnimationAction::WakeUp => vec![BotEvent::EntityWake(entity)],
                    // upstream maps animation 3 to `entityEat`; modern
                    // protocol says this is the off-hand swing (see parity)
                    AnimationAction::SwingOffHand => vec![BotEvent::EntityEat(entity)],
                    AnimationAction::CriticalHit => vec![BotEvent::EntityCriticalEffect(entity)],
                    AnimationAction::MagicCriticalHit => {
                        vec![BotEvent::EntityMagicCriticalEffect(entity)]
                    }
                }
            }
            EntityEvent(p) => {
                let Some(entity) = self.resolve(client, p.entity_id) else {
                    return vec![];
                };
                match entity_status_kind(p.event_id) {
                    Some(StatusKind::Hurt) => vec![BotEvent::EntityHurt {
                        entity,
                        source: None,
                    }],
                    Some(StatusKind::Dead) => vec![BotEvent::EntityDead(entity)],
                    Some(StatusKind::Taming) => vec![BotEvent::EntityTaming(entity)],
                    Some(StatusKind::Tamed) => vec![BotEvent::EntityTamed(entity)],
                    Some(StatusKind::ShakingOffWater) => {
                        vec![BotEvent::EntityShakingOffWater(entity)]
                    }
                    Some(StatusKind::EatingGrass) => vec![BotEvent::EntityEatingGrass(entity)],
                    Some(StatusKind::HandSwap) => {
                        // upstream swaps the stored equipment's main/off hand
                        if let Some(equipment) = self.equipment.get_mut(&p.entity_id) {
                            let main = equipment.remove(&EquipmentSlot::Mainhand);
                            let off = equipment.remove(&EquipmentSlot::Offhand);
                            if let Some(item) = off {
                                equipment.insert(EquipmentSlot::Mainhand, item);
                            }
                            if let Some(item) = main {
                                equipment.insert(EquipmentSlot::Offhand, item);
                            }
                        }
                        vec![BotEvent::EntityHandSwap(entity)]
                    }
                    None => vec![],
                }
            }
            DamageEvent(p) => {
                let Some(entity) = self.resolve(client, p.entity_id) else {
                    return vec![];
                };
                let source = p
                    .source_cause_id
                    .0
                    .map(MinecraftEntityId::from)
                    .and_then(|id| self.resolve(client, id));
                vec![BotEvent::EntityHurt { entity, source }]
            }
            SetEquipment(p) => {
                let slots = self.equipment.entry(p.entity_id).or_default();
                for (slot, item) in &p.slots.slots {
                    slots.insert(*slot, item.clone());
                }
                self.resolve(client, p.entity_id)
                    .map(BotEvent::EntityEquip)
                    .into_iter()
                    .collect()
            }
            UpdateAttributes(p) => {
                let attributes = self.attributes.entry(p.entity_id).or_default();
                for snapshot in &p.values {
                    attributes.insert(snapshot.attribute, snapshot.clone());
                }
                self.resolve(client, p.entity_id)
                    .map(BotEvent::EntityAttributes)
                    .into_iter()
                    .collect()
            }
            UpdateMobEffect(p) => {
                self.effects
                    .entry(p.entity_id)
                    .or_default()
                    .insert(p.mob_effect, p.data.clone());
                self.resolve(client, p.entity_id)
                    .map(|entity| BotEvent::EntityEffect {
                        entity,
                        effect: p.mob_effect,
                        data: p.data.clone(),
                    })
                    .into_iter()
                    .collect()
            }
            RemoveMobEffect(p) => {
                // upstream emits the tracked effect object and falls back to
                // `{id, amplifier: -1, duration: -1}` when the effect was
                // never seen — the remove packet carries only the kind.
                let data = self
                    .effects
                    .get_mut(&p.entity_id)
                    .and_then(|effects| effects.remove(&p.effect))
                    .unwrap_or(MobEffectData {
                        amplifier: -1,
                        duration: -1,
                        ..Default::default()
                    });
                self.resolve(client, p.entity_id)
                    .map(|entity| BotEvent::EntityEffectEnd {
                        entity,
                        effect: p.effect,
                        data,
                    })
                    .into_iter()
                    .collect()
            }
            TakeItemEntity(p) => vec![BotEvent::PlayerCollect {
                collector: self.resolve(client, p.player_id),
                collected: self.resolve(client, MinecraftEntityId::from(p.item_id)),
            }],
            SetEntityLink(p) => {
                let Some(entity) = self.resolve(client, p.source_id) else {
                    return vec![];
                };
                if p.dest_id == MinecraftEntityId(-1) {
                    let vehicle = self
                        .vehicle_of
                        .remove(&p.source_id)
                        .and_then(|id| self.resolve(client, id));
                    vec![BotEvent::EntityDetach { entity, vehicle }]
                } else {
                    self.vehicle_of.insert(p.source_id, p.dest_id);
                    let vehicle = self.resolve(client, p.dest_id);
                    vec![BotEvent::EntityAttach { entity, vehicle }]
                }
            }
            SetPassengers(p) => self.set_passengers(client, p.vehicle, &p.passengers, self_id),
            ChangeDifficulty(p) => {
                self.difficulty = Some(p.difficulty);
                vec![]
            }
            _ => vec![],
        }
    }

    fn moved(&mut self, client: &azalea::Client, id: MinecraftEntityId) -> Vec<BotEvent> {
        self.resolve(client, id)
            .map(BotEvent::EntityMoved)
            .into_iter()
            .collect()
    }

    /// `entity_metadata` handling: `entityUpdate` plus the derived events
    /// upstream extracts from metadata (sleep/crouch/elytra/item drop and the
    /// self-targeted firework).
    fn metadata_update(
        &mut self,
        client: &azalea::Client,
        id: MinecraftEntityId,
        packed_items: &[EntityDataItem],
        self_id: Option<MinecraftEntityId>,
    ) -> Vec<BotEvent> {
        let Some(entity) = self.resolve(client, id) else {
            return vec![];
        };
        let mut out = vec![BotEvent::EntityUpdate(entity.clone())];

        // upstream emits `itemDrop` whenever the metadata packet carries an
        // item_stack-typed entry
        if packed_items
            .iter()
            .any(|item| matches!(item.value, EntityDataValue::ItemStack(_)))
        {
            out.push(BotEvent::ItemDrop(entity.clone()));
        }

        // upstream emits entityCrouch/entitySleep on every metadata packet
        // carrying the flag; only the *release* transitions (uncrouch) and
        // elytra-start are edge-detected
        let meta = self.meta.entry(id).or_default();
        let shift_down = entity
            .get_component::<AbstractEntityShiftKeyDown>()
            .map(|f| f.0)
            .unwrap_or(false);
        let fall_flying = entity
            .get_component::<FallFlying>()
            .map(|f| f.0)
            .unwrap_or(false);
        let sleeping = entity
            .get_component::<Pose>()
            .map(|pose| *pose == Pose::Sleeping)
            .unwrap_or(false)
            || entity
                .get_component::<SleepingPos>()
                .map(|pos| pos.0.is_some())
                .unwrap_or(false);

        if shift_down {
            out.push(BotEvent::EntityCrouch(entity.clone()));
        } else if meta.shift_key_down {
            out.push(BotEvent::EntityUncrouch(entity.clone()));
        }
        if sleeping {
            out.push(BotEvent::EntitySleep(entity.clone()));
        }
        if fall_flying && !meta.fall_flying {
            out.push(BotEvent::EntityElytraFlew(entity.clone()));
        }
        if !fall_flying && meta.fall_flying && Some(id) == self_id {
            // upstream clears the firework boost when we stop elytra flying
            self.firework_rocket_duration = 0;
            self.seen_fireworks.clear();
        }
        *meta = EntityMeta {
            shift_key_down: shift_down,
            fall_flying,
        };

        // firework rockets attached to us feed `usedFirework` +
        // `bot.fireworkRocketDuration` (upstream reads the metadata the same way)
        if entity
            .get_component::<EntityKindComponent>()
            .map(|kind| kind.0)
            == Some(EntityKind::FireworkRocket)
            && self.seen_fireworks.insert(id)
            && entity
                .get_component::<AttachedToTarget>()
                .and_then(|target| (target.0).0)
                .map(|target| Some(MinecraftEntityId::from(target)) == self_id)
                .unwrap_or(false)
        {
            let flight = entity
                .get_component::<FireworksItem>()
                .and_then(|item| {
                    item.0
                        .get_component::<Fireworks>()
                        .map(|f| f.flight_duration)
                })
                .unwrap_or(1)
                .max(0);
            // upstream: 10 * (flight + 1) + rand(0..6) + rand(0..7); the
            // random jitter is derived deterministically from the entity id
            let jitter = (id.0 as u32).wrapping_mul(2654435761) % 6
                + (id.0 as u32).wrapping_mul(2246822519) % 7;
            self.firework_rocket_duration = (10 * (flight + 1)) as u32 + jitter;
            out.push(BotEvent::UsedFirework(id));
        }

        out
    }

    /// `c_set_passengers` — update vehicle links and emit `mount`/`dismount`
    /// when our own ride changes (upstream semantics; `entityAttach` is only
    /// for leashes, see `SetEntityLink`).
    fn set_passengers(
        &mut self,
        client: &azalea::Client,
        vehicle: MinecraftEntityId,
        passengers: &[MinecraftEntityId],
        self_id: Option<MinecraftEntityId>,
    ) -> Vec<BotEvent> {
        let old = self.passengers_of.insert(vehicle, passengers.to_vec());
        let mut out = Vec::new();
        let was = old.unwrap_or_default();
        for id in passengers.iter().filter(|p| !was.contains(p)) {
            self.vehicle_of.insert(*id, vehicle);
            if Some(*id) == self_id {
                out.push(BotEvent::Mounted {
                    vehicle: self.resolve(client, vehicle),
                });
            }
        }
        for id in was.iter().filter(|p| !passengers.contains(p)) {
            self.vehicle_of.remove(id);
            if Some(*id) == self_id {
                out.push(BotEvent::Dismounted {
                    vehicle: self.resolve(client, vehicle),
                });
            }
        }
        out
    }
}

/// mineflayer's `entityStatusEvents` table (entities.js) — `c_entity_event`
/// status codes to the emitted event name.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StatusKind {
    Hurt,
    Dead,
    Taming,
    Tamed,
    ShakingOffWater,
    EatingGrass,
    HandSwap,
}

fn entity_status_kind(event_id: u8) -> Option<StatusKind> {
    Some(match event_id {
        2 => StatusKind::Hurt,
        3 => StatusKind::Dead,
        6 => StatusKind::Taming,
        7 => StatusKind::Tamed,
        8 => StatusKind::ShakingOffWater,
        10 => StatusKind::EatingGrass,
        55 => StatusKind::HandSwap,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Queries and properties
// ---------------------------------------------------------------------------

/// The mineflayer `Player` object — a tab-list entry plus its world entity.
///
/// `info` is azalea's [`PlayerInfo`] (`profile.uuid`, `gamemode`, `latency`,
/// `display_name`, chat session); `entity` is the player's in-world entity
/// when it's spawned and loaded.
#[derive(Clone)]
pub struct Player {
    /// The full tab-list entry.
    pub info: PlayerInfo,
    /// The player entity in the world, if spawned and loaded.
    pub entity: Option<EntityRef>,
}

impl Player {
    /// `player.username`.
    pub fn username(&self) -> &str {
        &self.info.profile.name
    }
    /// `player.displayName` — the tab-list display name, if the server set a
    /// custom one (upstream falls back to the username).
    pub fn display_name(&self) -> Option<&FormattedText> {
        self.info.display_name.as_deref()
    }
    /// `player.gamemode`.
    pub fn gamemode(&self) -> azalea::core::game_type::GameMode {
        self.info.gamemode
    }
    /// `player.ping` — latency in milliseconds.
    pub fn ping(&self) -> i32 {
        self.info.latency
    }
    /// `player.skinData` — decoded from the `textures` profile property.
    pub fn skin_data(&self) -> Option<SkinData> {
        skin_data(&self.info.profile)
    }
}

/// `Entity.skinData`/`Player.skinData` — parsed from the signed `textures`
/// game-profile property.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinData {
    /// Skin texture URL.
    pub url: String,
    /// `"slim"` for the slim (Alex) model; `None` for classic.
    pub model: Option<String>,
    /// Cape texture URL, if the account has one.
    pub cape_url: Option<String>,
}

/// Filter for [`Bot::find_player`]/[`Bot::find_players`], mirroring upstream's
/// `string | RegExp | fn | null` filter argument.
#[derive(Clone, Copy)]
pub enum PlayerFilter<'a> {
    /// Match every player entity.
    Any,
    /// Exact username, case-insensitive (upstream `string` filters).
    Username(&'a str),
    /// Username must match the regex (upstream `RegExp` filters).
    Matches(&'a Regex),
    /// Arbitrary predicate over the entity (upstream `function` filters).
    Predicate(&'a dyn Fn(&EntityRef) -> bool),
}

fn username_matches(filter: &PlayerFilter, name: &str) -> bool {
    match filter {
        PlayerFilter::Any => true,
        PlayerFilter::Username(want) => name.eq_ignore_ascii_case(want),
        PlayerFilter::Matches(re) => re.is_match(name),
        PlayerFilter::Predicate(_) => true,
    }
}

/// Convenience methods for [`EntityRef`] that upstream puts on the `Entity`
/// object itself.
pub trait EntityExt {
    /// `Entity.yaw`/`Entity.pitch` — the entity's look direction.
    fn look_direction(&self) -> Option<LookDirection>;
    /// `Entity.velocity`.
    fn velocity(&self) -> Option<Vec3>;
    /// `Entity.onGround`.
    fn on_ground(&self) -> Option<bool>;
    /// `Entity.username` — the player's name, on player entities.
    fn username(&self) -> Option<String>;
    /// `Entity.skinData` — decoded textures property, on player entities.
    fn skin_data(&self) -> Option<SkinData>;
}

impl EntityExt for EntityRef {
    fn look_direction(&self) -> Option<LookDirection> {
        self.get_component::<LookDirection>().map(|l| *l)
    }
    fn velocity(&self) -> Option<Vec3> {
        self.get_component::<Physics>().map(|p| p.velocity)
    }
    fn on_ground(&self) -> Option<bool> {
        self.get_component::<Physics>().map(|p| p.on_ground())
    }
    fn username(&self) -> Option<String> {
        self.get_component::<GameProfileComponent>()
            .map(|p| p.name.clone())
    }
    fn skin_data(&self) -> Option<SkinData> {
        skin_data(&self.get_component::<GameProfileComponent>()?.0)
    }
}

impl Bot {
    /// `bot.entity` — the bot's own entity as an azalea [`azalea::EntityRef`],
    /// usable anywhere a world entity is expected.
    pub fn entity(&self) -> azalea::EntityRef {
        self.client.entity()
    }

    /// `bot.entity.position` convenience — the bot's feet position.
    /// `Vec3::ZERO` until the first position arrives (azalea's own default).
    pub fn position(&self) -> azalea::Vec3 {
        self.client
            .get_component::<Position>()
            .map(|p| **p)
            .unwrap_or_default()
    }

    /// `bot.entities` — every entity in view, keyed by protocol entity id
    /// (like upstream's `{[id]: Entity}`), including the bot itself.
    pub fn entities(&self) -> HashMap<MinecraftEntityId, EntityRef> {
        self.client
            .nearest_entities_by::<(), With<Position>>(|_: ()| true)
            .iter()
            .filter_map(|e| {
                e.get_component::<MinecraftEntityId>()
                    .map(|id| (*id, e.clone()))
            })
            .collect()
    }

    /// `bot.nearestEntity(match?)` — the closest entity that isn't us and
    /// satisfies `matcher`.
    pub fn nearest_entity(&self, mut matcher: impl FnMut(&EntityRef) -> bool) -> Option<EntityRef> {
        let own = self.client.entity;
        self.client
            .nearest_entities_by::<(), With<Position>>(|_: ()| true)
            .into_iter()
            .find(|e| e.id() != own && matcher(e))
    }

    /// `bot.findPlayer`/`bot.findPlayers` — player entities matching
    /// `filter` (upstream returns entities, not tab-list entries).
    pub fn find_players(&self, filter: PlayerFilter) -> Vec<EntityRef> {
        self.client
            .nearest_entities_by::<(), With<azalea::entity::metadata::Player>>(|_: ()| true)
            .into_iter()
            .filter(|e| match &filter {
                PlayerFilter::Predicate(f) => f(e),
                _ => e
                    .get_component::<GameProfileComponent>()
                    .map(|p| username_matches(&filter, &p.name))
                    .unwrap_or(false),
            })
            .collect()
    }

    /// The first player entity matching `filter`, nearest first.
    pub fn find_player(&self, filter: PlayerFilter) -> Option<EntityRef> {
        self.find_players(filter).into_iter().next()
    }

    /// `bot.players` — the tab list, keyed by username.
    pub fn players(&self) -> HashMap<String, Player> {
        // Snapshot the tab list before resolving uuids: the component guard
        // holds an ECS read lock and `entity_by_uuid` re-locks it — with a
        // writer queued in between, parking_lot deadlocks the second read.
        let infos: Vec<PlayerInfo> = self
            .client
            .get_component::<azalea::local_player::TabList>()
            .map(|tab_list| tab_list.values().cloned().collect())
            .unwrap_or_default();
        infos
            .into_iter()
            .map(|info| {
                let entity = self.client.entity_by_uuid(info.uuid);
                (info.profile.name.clone(), Player { info, entity })
            })
            .collect()
    }

    /// `bot.player` — the [`Player`] for the bot itself; `None` until the tab
    /// list entry arrives.
    pub fn player(&self) -> Option<Player> {
        let uuid = self
            .client
            .get_component::<GameProfileComponent>()
            .map(|p| p.uuid)?;
        self.player_by_uuid(&uuid.to_string())
    }

    /// `bot._playerFromUUID` — tab-list lookup by hyphenated UUID string.
    pub fn player_by_uuid(&self, uuid: &str) -> Option<Player> {
        self.players()
            .into_values()
            .find(|p| p.info.uuid.to_string() == uuid)
    }

    /// `bot.uuidToUsername` — `{[uuid]: username}` over the tab list. Keys
    /// are hyphenated UUID strings, like upstream.
    pub fn uuid_to_username(&self) -> HashMap<String, String> {
        self.client
            .get_component::<azalea::local_player::TabList>()
            .map(|tab_list| {
                tab_list
                    .values()
                    .map(|info| (info.uuid.to_string(), info.profile.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The entity we are riding (`bot.vehicle`); tracked from
    /// `c_set_passengers`/`c_set_entity_link`.
    pub fn vehicle(&self) -> Option<EntityRef> {
        self.entity_vehicle(&self.entity())
    }

    /// `Entity.vehicle` for an arbitrary entity.
    pub fn entity_vehicle(&self, entity: &EntityRef) -> Option<EntityRef> {
        let id = *entity.get_component::<MinecraftEntityId>()?;
        let vehicle = *self.entities.read().unwrap().vehicle_of.get(&id)?;
        self.entities
            .write()
            .unwrap()
            .resolve(&self.client, vehicle)
    }

    /// `entity.equipment` — the entity's equipment by slot, tracked from
    /// `c_set_equipment` (azalea drops that packet). Empty for entities that
    /// never sent equipment.
    pub fn equipment(&self, entity: &EntityRef) -> HashMap<EquipmentSlot, ItemStack> {
        // resolve the id before touching the tracker — holding the
        // component guard across the tracker read inverts the bridge's
        // (tracker-write → ecs-read) lock order and deadlocks
        let id = entity.get_component::<MinecraftEntityId>().map(|id| *id);
        id.and_then(|id| self.entities.read().unwrap().equipment.get(&id).cloned())
            .unwrap_or_default()
    }

    /// `bot.fireworkRocketDuration` — estimated remaining boost ticks from
    /// the last `usedFirework`; `0` when none is active.
    pub fn firework_rocket_duration(&self) -> u32 {
        self.entities.read().unwrap().firework_rocket_duration
    }

    // -- actions ---------------------------------------------------------

    /// `bot.attack(entity, swing?)` — with `swing` it queues the attack
    /// through azalea (which sends attack + arm swing on the next game tick,
    /// like vanilla input); without it sends a bare attack packet.
    pub fn attack(&self, entity: &EntityRef, swing: bool) {
        if swing {
            self.client.attack(entity.id());
        } else {
            // copy the id out and drop the component guard — write_packet
            // locks the ECS and a read guard held across it deadlocks
            let entity_id = entity.get_component::<MinecraftEntityId>().map(|id| *id);
            if let Some(entity_id) = entity_id {
                self.client.write_packet(ServerboundAttack { entity_id });
            }
        }
    }

    /// `bot.swingArm(hand?)` — `InteractionHand::MainHand` is upstream's
    /// `'right'`. (Upstream's `showHand=false` has no packet-level meaning in
    /// this protocol version; see api-parity.)
    pub fn swing_arm(&self, hand: InteractionHand) {
        self.client.write_packet(ServerboundSwing { hand });
    }

    /// `bot.activateEntity(entity)` — right-click an entity (use item /
    /// talk to villager / board minecart).
    pub fn activate_entity(&self, entity: &EntityRef) {
        self.client.entity_interact(entity.id());
    }

    /// `bot.activateEntityAt(entity, pos)` — right-click at a specific point
    /// on the entity's hitbox (e.g. armor stands).
    pub fn activate_entity_at(&self, entity: &EntityRef, pos: Vec3) {
        self.client
            .ecs
            .write()
            .trigger(azalea::interact::EntityInteractEvent {
                client: self.client.entity,
                target: entity.id(),
                location: Some(pos),
            });
    }

    /// `bot.useOn(target)` — saddle/shears-style use. Upstream sends the
    /// same interact packet as `activateEntity`.
    pub fn use_on(&self, entity: &EntityRef) {
        self.client.entity_interact(entity.id());
    }

    /// `bot.mount(entity)` — enter a vehicle. Upstream sends the same
    /// interact packet as `activateEntity`.
    pub fn mount(&self, entity: &EntityRef) {
        self.client.entity_interact(entity.id());
    }

    /// `bot.dismount()` — leave the vehicle. Emits `BotEvent::Error` when
    /// not mounted, like upstream's `'error'` event. Sends a shift-press
    /// input, the dismount action in this protocol (see api-parity for the
    /// upstream `player_input` quirk).
    pub fn dismount(&self) {
        if self.vehicle().is_none() {
            self.emit_event(BotEvent::Error("dismount: not mounted".to_string()));
            return;
        }
        self.client.write_packet(ServerboundPlayerInput {
            shift: true,
            ..Default::default()
        });
        // azalea sends player_input diffs against its own LastSentInput, so a
        // matching release restores the on-wire state without a second packet
        self.client.write_packet(ServerboundPlayerInput::default());
        // edge: if azalea already thinks shift is held (trying_to_crouch),
        // the press packet diffs to nothing — the sneak never reaches the
        // server; matching upstream's raw write is still the closest analog
        // (upstream has the same desync when its sneak flag drifts).
    }

    /// `bot.moveVehicle(left, forward)` — steer input for the ridden
    /// vehicle (`-1`/`1`, upstream semantics).
    pub fn move_vehicle(&self, left: i8, forward: i8) {
        self.client.write_packet(ServerboundPlayerInput {
            forward: forward > 0,
            backward: forward < 0,
            left: left > 0,
            right: left < 0,
            ..Default::default()
        });
    }

    /// `bot.elytraFly()` — send start-fall-flying. Mirrors upstream's
    /// preconditions (not already flying, airborne, not in water, no
    /// levitation). The elytra-equipped check needs the inventory surface —
    /// deferred to that unit; the server rejects the packet without one.
    pub fn elytra_fly(&self) -> Result<()> {
        let me = self.entity();
        if me
            .get_component::<FallFlying>()
            .map(|f| f.0)
            .unwrap_or(false)
        {
            return Err(Error::Operation("already elytra flying".to_string()));
        }
        let physics = me.get_component::<Physics>().ok_or(Error::NotConnected)?;
        // extract before the later get_component calls — the guard holds an
        // ECS read lock and nested reads deadlock behind a queued writer
        let (on_ground, in_water) = (physics.on_ground(), physics.is_in_water());
        drop(physics);
        if on_ground {
            return Err(Error::Operation("unable to fly from ground".to_string()));
        }
        if in_water {
            return Err(Error::Operation(
                "unable to elytra fly while in water".to_string(),
            ));
        }
        if me
            .get_component::<ActiveEffects>()
            .map(|effects| effects.0.contains_key(&MobEffect::Levitation))
            .unwrap_or(false)
        {
            return Err(Error::Operation(
                "unable to elytra fly with levitation effect".to_string(),
            ));
        }
        let id = *self
            .client
            .get_component::<MinecraftEntityId>()
            .ok_or(Error::NotConnected)?;
        self.client.write_packet(ServerboundPlayerCommand {
            id,
            action: PlayerCommandAction::StartFallFlying,
            data: 0,
        });
        Ok(())
    }

    /// `bot.getExplosionDamages(entity, sourcePos, power, rawDamages?)` —
    /// estimate the damage an explosion of `power` at `source_pos` would deal
    /// to `entity`. Port of upstream's formula: exposure is raycast against
    /// loaded chunks; armor comes from attributes tracked off
    /// `c_update_attributes` (azalea doesn't store armor attributes).
    /// Returns `None` when the entity's position or armor data is unknown
    /// (upstream: `null`).
    pub fn get_explosion_damages(
        &self,
        entity: &EntityRef,
        source_pos: Vec3,
        power: f64,
        raw: bool,
    ) -> Option<f32> {
        const DAMAGE_MULTIPLIER: f64 = 7.0;
        let pos = entity.get_component::<Position>().map(|p| **p)?;
        let distance = pos.distance_to(source_pos);
        let radius = 2.0 * power;
        if distance >= radius {
            return Some(0.0);
        }
        let exposure = self.explosion_exposure(pos, source_pos);
        let impact = (1.0 - distance / radius) * exposure;
        let mut damages = ((impact * impact + impact) * DAMAGE_MULTIPLIER * power + 1.0).floor();

        if !raw {
            let id = *entity.get_component::<MinecraftEntityId>()?;
            let attributes = self.entities.read().unwrap().attributes.get(&id).cloned();
            let armor = attributes
                .as_ref()
                .and_then(|a| a.get(&Attribute::Armor))
                .map(attribute_value)?;
            let toughness = attributes
                .as_ref()
                .and_then(|a| a.get(&Attribute::ArmorToughness))
                .map(attribute_value)
                .unwrap_or(0.0);
            damages = damage_after_absorb(damages, armor, toughness);
            if entity
                .get_component::<EntityKindComponent>()
                .map(|kind| kind.0)
                == Some(EntityKind::Player)
            {
                let scale = match self
                    .entities
                    .read()
                    .unwrap()
                    .difficulty
                    .unwrap_or(Difficulty::Normal)
                {
                    Difficulty::Peaceful => 0.0,
                    Difficulty::Easy => 0.5,
                    Difficulty::Normal => 1.0,
                    Difficulty::Hard => 1.5,
                };
                damages *= scale;
            }
        }
        Some(damages.floor() as f32)
    }

    /// Upstream `calcExposure` — sample a grid of points over the entity's
    /// 0.6×1.8 bounding box and count how many have line of sight to the
    /// explosion.
    fn explosion_exposure(&self, player_pos: Vec3, explosion_pos: Vec3) -> f64 {
        let dx: f64 = 1.0 / (0.6 * 2.0 + 1.0);
        let dy: f64 = 1.0 / (1.8 * 2.0 + 1.0);
        let dz: f64 = 1.0 / (0.6 * 2.0 + 1.0);
        let d3 = (1.0 - (1.0 / dx).floor() * dx) / 2.0;
        let d4 = (1.0 - (1.0 / dz).floor() * dz) / 2.0;

        let world = self.client.world();
        let world = world.read();
        let mut sampled = 0u32;
        let mut exposed = 0u32;
        let mut y = player_pos.y;
        while y <= player_pos.y + 1.8 {
            let mut x = player_pos.x - 0.3 + d3;
            while x <= player_pos.x + 0.3 {
                let mut z = player_pos.z - 0.3 + d4;
                while z <= player_pos.z + 0.3 {
                    let hit = clip(
                        &world.chunks,
                        ClipContext {
                            from: explosion_pos,
                            to: Vec3::new(x, y, z),
                            block_shape_type: BlockShapeType::Collider,
                            fluid_pick_type: FluidPickType::None,
                        },
                    );
                    if hit.miss {
                        exposed += 1;
                    }
                    sampled += 1;
                    z += 0.6 * dz;
                }
                x += 0.6 * dx;
            }
            y += 1.8 * dy;
        }
        exposed as f64 / sampled.max(1) as f64
    }
}

// ---------------------------------------------------------------------------
// Free helpers (pure — unit-tested)
// ---------------------------------------------------------------------------

/// Upstream `getAttributeValue` — fold an attribute snapshot's base and
/// modifiers: op0 adds to base, op1 scales by base, op2 compounds.
fn attribute_value(snapshot: &AttributeSnapshot) -> f64 {
    use azalea::core::attribute_modifier_operation::AttributeModifierOperation::*;
    let mut base = snapshot.base;
    for m in &snapshot.modifiers {
        if m.operation == AddValue {
            base += m.amount;
        }
    }
    let mut total = base;
    for m in &snapshot.modifiers {
        if m.operation == AddMultipliedBase {
            total += base * m.amount;
        }
    }
    for m in &snapshot.modifiers {
        if m.operation == AddMultipliedTotal {
            total += total * m.amount;
        }
    }
    total
}

/// Upstream `getDamageAfterAbsorb` (armor damage reduction).
fn damage_after_absorb(damages: f64, armor: f64, toughness: f64) -> f64 {
    let factor = 2.0 + toughness / 4.0;
    let effective = (armor - damages / factor).max(armor * 0.2).min(20.0);
    damages * (1.0 - effective / 25.0)
}

/// Decode the `textures` profile property into [`SkinData`]
/// (upstream `extractSkinInformation`).
fn skin_data(profile: &azalea::auth::game_profile::GameProfile) -> Option<SkinData> {
    static SKIN_URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""SKIN"\s*:\s*\{[^}]*"url"\s*:\s*"([^"]+)""#).unwrap());
    static SKIN_MODEL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""SKIN"\s*:\s*\{[^}]*"metadata"\s*:\s*\{[^}]*"model"\s*:\s*"([^"]+)""#)
            .unwrap()
    });
    static CAPE_URL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""CAPE"\s*:\s*\{[^}]*"url"\s*:\s*"([^"]+)""#).unwrap());

    let textures = profile.properties.map.get("textures")?;
    let json = String::from_utf8(base64_decode(&textures.value)?).ok()?;
    Some(SkinData {
        url: SKIN_URL.captures(&json)?.get(1)?.as_str().to_string(),
        model: SKIN_MODEL
            .captures(&json)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string()),
        cape_url: CAPE_URL
            .captures(&json)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string()),
    })
}

/// Minimal standard-alphabet base64 decode (the `textures` property is
/// base64-encoded JSON). Returns `None` on invalid input.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    static TABLE: LazyLock<[u8; 256]> = LazyLock::new(|| {
        let mut table = [0xFFu8; 256];
        for (i, c) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
            .iter()
            .enumerate()
        {
            table[*c as usize] = i as u8;
        }
        table
    });
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &byte in input.as_bytes() {
        if byte == b'=' {
            break;
        }
        let value = *TABLE.get(byte as usize)?;
        if value == 0xFF {
            return None;
        }
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use azalea::{
        core::attribute_modifier_operation::AttributeModifierOperation,
        inventory::components::AttributeModifier,
        protocol::packets::game::{
            c_animate::ClientboundAnimate, c_update_attributes::AttributeSnapshot,
        },
    };

    use super::*;

    fn snapshot(base: f64, modifiers: Vec<AttributeModifier>) -> AttributeSnapshot {
        AttributeSnapshot {
            attribute: Attribute::Armor,
            base,
            modifiers,
        }
    }

    fn modifier(amount: f64, operation: AttributeModifierOperation) -> AttributeModifier {
        AttributeModifier {
            id: azalea::Identifier::new("test"),
            amount,
            operation,
        }
    }

    #[test]
    fn entity_status_table_matches_upstream() {
        // upstream's entityStatusEvents mapping, verbatim
        assert_eq!(entity_status_kind(2), Some(StatusKind::Hurt));
        assert_eq!(entity_status_kind(3), Some(StatusKind::Dead));
        assert_eq!(entity_status_kind(6), Some(StatusKind::Taming));
        assert_eq!(entity_status_kind(7), Some(StatusKind::Tamed));
        assert_eq!(entity_status_kind(8), Some(StatusKind::ShakingOffWater));
        assert_eq!(entity_status_kind(10), Some(StatusKind::EatingGrass));
        assert_eq!(entity_status_kind(55), Some(StatusKind::HandSwap));
        assert_eq!(entity_status_kind(4), None);
        assert_eq!(entity_status_kind(0), None);
        assert_eq!(entity_status_kind(54), None);
    }

    #[test]
    fn attribute_value_folds_modifiers_like_upstream() {
        // no modifiers -> base
        assert_eq!(attribute_value(&snapshot(20.0, vec![])), 20.0);
        // op0 (add value): base + amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(2.0, AttributeModifierOperation::AddValue)]
            )),
            22.0
        );
        // op1 (add multiplied base): base + base*amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(0.5, AttributeModifierOperation::AddMultipliedBase)]
            )),
            30.0
        );
        // op2 (add multiplied total): total + total*amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(
                    0.5,
                    AttributeModifierOperation::AddMultipliedTotal
                )]
            )),
            30.0
        );
        // combined: (20 + 2) + 22*0.5 then *2 = 66
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![
                    modifier(2.0, AttributeModifierOperation::AddValue),
                    modifier(0.5, AttributeModifierOperation::AddMultipliedBase),
                    modifier(1.0, AttributeModifierOperation::AddMultipliedTotal),
                ]
            )),
            66.0
        );
    }

    #[test]
    fn armor_absorb_matches_upstream_formula() {
        // iron armor: 15 armor, 0 toughness vs 15 damage ->
        // effective = min(max(15 - 15/2, 3), 20) = 7.5; 15 * (1 - 7.5/25) = 10.5
        assert_eq!(damage_after_absorb(15.0, 15.0, 0.0), 10.5);
        // diamond: 20 armor, 8 toughness vs 15 -> factor 4; eff = min(max(20-3.75,4),20)=16.25; 15*(1-.65)=5.25
        assert_eq!(damage_after_absorb(15.0, 20.0, 8.0), 5.25);
        // armor floor: armor*0.2 when the reduction term goes below it
        assert_eq!(damage_after_absorb(100.0, 10.0, 0.0), 92.0);
    }

    #[test]
    fn base64_decode_vectors() {
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(base64_decode(""), Some(vec![]));
        assert_eq!(base64_decode("###"), None);
        // full payload round-trip
        let json = r#"{"timestamp":0,"profileId":"abc","profileName":"faye","textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/s","metadata":{"model":"slim"}}}}"#;
        let b64 = "eyJ0aW1lc3RhbXAiOjAsInByb2ZpbGVJZCI6ImFiYyIsInByb2ZpbGVOYW1lIjoiZmF5ZSIsInRleHR1cmVzIjp7IlNLSU4iOnsidXJsIjoiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS9zIiwibWV0YWRhdGEiOnsibW9kZWwiOiJzbGltIn19fX0=";
        assert_eq!(
            String::from_utf8(base64_decode(b64).unwrap()).unwrap(),
            json
        );
    }

    #[test]
    fn skin_data_parses_mojang_payload() {
        // real-shaped textures payload (base64 of the JSON above)
        let b64 = "eyJ0aW1lc3RhbXAiOjAsInByb2ZpbGVJZCI6ImFiYyIsInByb2ZpbGVOYW1lIjoiZmF5ZSIsInRleHR1cmVzIjp7IlNLSU4iOnsidXJsIjoiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS9zIiwibWV0YWRhdGEiOnsibW9kZWwiOiJzbGltIn19LCJDQVBFIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvYyJ9fX0=";
        let json = r#"{"timestamp":0,"profileId":"abc","profileName":"faye","textures":{"SKIN":{"url":"http://textures.minecraft.net/texture/s","metadata":{"model":"slim"}},"CAPE":{"url":"http://textures.minecraft.net/texture/c"}}}"#;
        assert_eq!(
            String::from_utf8(base64_decode(b64).unwrap()).unwrap(),
            json
        );
        let mut properties = azalea::auth::game_profile::GameProfileProperties::default();
        properties.map.insert(
            "textures".to_string(),
            azalea::auth::game_profile::ProfilePropertyValue {
                value: b64.to_string(),
                signature: None,
            },
        );
        let profile = azalea::auth::game_profile::GameProfile {
            uuid: Default::default(),
            name: "faye".to_string(),
            properties: std::sync::Arc::new(properties),
        };
        assert_eq!(
            skin_data(&profile),
            Some(SkinData {
                url: "http://textures.minecraft.net/texture/s".to_string(),
                model: Some("slim".to_string()),
                cape_url: Some("http://textures.minecraft.net/texture/c".to_string()),
            })
        );
        // no textures property -> None
        let bare = azalea::auth::game_profile::GameProfile::new(Default::default(), "x".into());
        assert_eq!(skin_data(&bare), None);
    }

    #[test]
    fn username_filters() {
        let re = Regex::new("^u4").unwrap();
        assert!(username_matches(&PlayerFilter::Any, "x"));
        assert!(username_matches(&PlayerFilter::Username("U4BOT"), "u4bot"));
        assert!(!username_matches(&PlayerFilter::Username("u4"), "u4bot"));
        assert!(username_matches(&PlayerFilter::Matches(&re), "u4bot"));
        assert!(!username_matches(&PlayerFilter::Matches(&re), "faye"));
    }

    #[test]
    fn forget_cleans_vehicle_and_passenger_maps() {
        let id = |i: i32| MinecraftEntityId(i);
        // mount: self appears in a vehicle's passenger list
        let mut tracker = EntityTracker::default();
        assert!(tracker.passengers_of.is_empty());
        // can't call set_passengers (needs a Client); exercise the diff logic
        // through the maps directly
        tracker.passengers_of.insert(id(1), vec![id(5)]);
        tracker.vehicle_of.insert(id(5), id(1));
        assert_eq!(tracker.vehicle_of.get(&id(5)), Some(&id(1)));
        // removal cleans the maps
        tracker.forget(Some(id(5)), id(1));
        assert!(!tracker.vehicle_of.contains_key(&id(5)));
        // removing a passenger cleans its vehicle link
        tracker.passengers_of.insert(id(2), vec![id(6)]);
        tracker.vehicle_of.insert(id(6), id(2));
        tracker.forget(None, id(6));
        assert!(tracker.passengers_of.get(&id(2)).unwrap().is_empty());
    }

    #[test]
    fn packet_after_self_despawn_emits_nothing() {
        let client = crate::test_util::despawned_client();
        let tracker = RwLock::new(EntityTracker::default());
        // teardown packets race the despawn of our own entity; the id
        // lookup must miss quietly instead of panicking.
        let packet = ClientboundGamePacket::Animate(ClientboundAnimate {
            id: MinecraftEntityId(1),
            action: AnimationAction::SwingMainHand,
        });
        assert!(observe_event(&tracker, &client, &Event::Packet(Arc::new(packet))).is_empty());
    }
}
