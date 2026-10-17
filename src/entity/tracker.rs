//! The entity-event reducer: packet decode into [`EntityTracker`] state
//! and the entity `BotEvent`s the engine does not raise itself.

use super::*;

// ---------------------------------------------------------------------------
// Event bridging (bridge.rs calls `observe_event` for every engine `Event`)
// ---------------------------------------------------------------------------

/// Entity-domain state the ECS doesn't keep, shared between the bridge task
/// (writer) and `Bot` handles (readers).
///
/// Keyed by [`MinecraftEntityId`] rather than ECS `Entity` so entries stay
/// meaningful for just-removed entities — the ECS side of a removed entity is
/// dropped by the engine before our packet events are even read.
#[derive(Default)]
pub(crate) struct EntityTracker {
    /// Entity ids we've resolved to an ECS `Entity`, kept after despawn so
    /// `entityGone`/`playerCollect` still produce an [`EntityRef`] handle.
    pub(super) known: HashMap<MinecraftEntityId, Entity>,
    /// Last flag-carried metadata state per entity, for edge detection on
    /// `entity_metadata` packets (crouch release, elytra start).
    pub(super) meta: HashMap<MinecraftEntityId, EntityMeta>,
    /// `c_set_equipment` contents per entity. The engine drops this packet
    /// (`debug!`-only handler), so equipment lives here.
    pub(super) equipment: HashMap<MinecraftEntityId, HashMap<EquipmentSlot, ItemStack>>,
    /// `c_update_attributes` contents per entity — the engine only stores the
    /// subset of attributes it uses (no armor), so the raw snapshots are
    /// tracked for [`Bot::get_explosion_damages`].
    pub(super) attributes: HashMap<MinecraftEntityId, HashMap<Attribute, AttributeSnapshot>>,
    /// Active mob effects per entity (`c_update_mob_effect` payload), so
    /// `entityEffectEnd` can report the effect's data like upstream's
    /// `entity.effects[id]` lookup does.
    pub(super) effects: HashMap<MinecraftEntityId, HashMap<MobEffect, MobEffectData>>,
    /// `c_set_passengers` passenger lists per vehicle.
    pub(super) passengers_of: HashMap<MinecraftEntityId, Vec<MinecraftEntityId>>,
    /// Passenger → vehicle link, fed by `c_set_passengers` and
    /// `c_set_entity_link` (leashes).
    pub(super) vehicle_of: HashMap<MinecraftEntityId, MinecraftEntityId>,
    /// Firework entity ids already counted for `usedFirework`/`fireworkRocketDuration`.
    pub(super) seen_fireworks: HashSet<MinecraftEntityId>,
    pub(super) firework_rocket_duration: u32,
    /// `c_change_difficulty` — needed for the explosion formula's player
    /// scaling. `None` until the server sends one (upstream's
    /// `bot.game.difficulty` is likewise only set after the game packet).
    pub(super) difficulty: Option<Difficulty>,
    /// Chat-signing keys per player uuid, from `c_player_info_update`'s
    /// `initialize_chat` action — the engine's [`PlayerInfo`] drops the field
    /// (`profileKeys` upstream).
    pub(super) chat_sessions: HashMap<uuid::Uuid, ProfileKeys>,
}

/// Last flag-carried snapshot per entity for the edge-detected events
/// (`entityUncrouch`, `entityElytraFlew`). Updated only when a packet
/// carries `shared_flags`, like upstream's `entity.crouching`/
/// `entity.elytraFlying`.
#[derive(Default)]
pub(super) struct EntityMeta {
    shift_key_down: bool,
    fall_flying: bool,
}

/// Feed one engine [`Event`] into the entity domain, returning the
/// [`BotEvent`]s it produced. Called by the bridge for *every* event, before
/// `translate` runs, so the returned events are emitted right after the
/// corresponding raw `BotEvent::Packet`/`Spawn`.
pub(crate) fn observe_event(
    tracker: &RwLock<EntityTracker>,
    client: &mineflayer_engine::Client,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        // lock rule (bot/mod.rs): the entity-tracker lock is held while
        // `handle_packet` takes its brief ECS reads — allowed direction.
        Event::Packet(packet) => tracker.write().handle_packet(client, packet.as_ref()),
        // `bot.on('spawn', () => bot.emit('entitySpawn', bot.entity))` upstream
        Event::Spawn => vec![BotEvent::EntitySpawn(client.entity())],
        // upstream resets `bot.entities` on login
        Event::Login => {
            tracker.write().clear();
            vec![]
        }
        _ => vec![],
    }
}

/// [`Client::entity_id_by_minecraft_id`](mineflayer_engine::Client::entity_id_by_minecraft_id)
/// without the panic — the engine resolves the id by `query_self`-ing our own
/// entity for `EntityIdIndex`, but teardown packets race the despawn of
/// that entity.
pub(crate) fn entity_id_by_minecraft_id(
    client: &mineflayer_engine::Client,
    id: MinecraftEntityId,
) -> Option<Entity> {
    client
        .try_query_self::<&EntityIdIndex, _>(|index| index.get_by_minecraft_entity(id))
        .ok()
        .flatten()
}

/// [`Client::entity_by_minecraft_id`](mineflayer_engine::Client::entity_by_minecraft_id)
/// without the panic — same despawn race as above.
pub(crate) fn entity_by_minecraft_id(
    client: &mineflayer_engine::Client,
    id: MinecraftEntityId,
) -> Option<EntityRef> {
    entity_id_by_minecraft_id(client, id).map(|e| client.entity_ref_for(e))
}

impl EntityTracker {
    pub(super) fn clear(&mut self) {
        *self = EntityTracker::default();
    }

    /// Drop every per-entity entry except the bot's own (`c_respawn`). The
    /// public behavior does not change — upstream clears `bot.entities`
    /// only on login, and these entries are unreachable once their
    /// entities are gone — but the maps no longer grow with each dimension
    /// change. Player-keyed state (`chat_sessions`) and `difficulty` are
    /// not per-dimension and stay.
    pub(super) fn forget_other_entities(&mut self, self_id: Option<MinecraftEntityId>) {
        let keep = |id: &MinecraftEntityId| Some(*id) == self_id;
        self.known.retain(|id, _| keep(id));
        self.meta.retain(|id, _| keep(id));
        self.equipment.retain(|id, _| keep(id));
        self.attributes.retain(|id, _| keep(id));
        self.effects.retain(|id, _| keep(id));
        self.passengers_of.clear();
        self.vehicle_of.clear();
        self.seen_fireworks.clear();
    }

    /// Resolve a protocol entity id to an [`EntityRef`], preferring the live
    /// ECS index and falling back to ids remembered before despawn.
    pub(super) fn resolve(
        &mut self,
        client: &mineflayer_engine::Client,
        id: MinecraftEntityId,
    ) -> Option<EntityRef> {
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
    pub(super) fn forget(
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
    /// after the engine's own packet handler, so the ECS already reflects the
    /// packet (indexes are populated for spawns, cleared for removals).
    pub(super) fn handle_packet(
        &mut self,
        client: &mineflayer_engine::Client,
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
            PlayerInfoUpdate(p) => {
                for entry in &p.entries {
                    // upstream rebuilds the player object on add_player —
                    // a re-add without chat keys clears them here too
                    if p.actions.add_player {
                        self.chat_sessions.remove(&entry.profile.uuid);
                    }
                    if let Some(session) = &entry.chat_session {
                        self.chat_sessions.insert(
                            entry.profile.uuid,
                            ProfileKeys {
                                public_key: session.profile_public_key.key.clone(),
                                signature: session.profile_public_key.key_signature.clone(),
                            },
                        );
                    }
                }
                vec![]
            }
            PlayerInfoRemove(p) => {
                for uuid in &p.profile_ids {
                    self.chat_sessions.remove(uuid);
                }
                vec![]
            }
            // a dimension change: the server sends no `RemoveEntities` for
            // the old dimension, so its entries would stay forever
            Respawn(_) => {
                self.forget_other_entities(self_id);
                vec![]
            }
            _ => vec![],
        }
    }

    pub(super) fn moved(
        &mut self,
        client: &mineflayer_engine::Client,
        id: MinecraftEntityId,
    ) -> Vec<BotEvent> {
        self.resolve(client, id)
            .map(BotEvent::EntityMoved)
            .into_iter()
            .collect()
    }

    /// `entity_metadata` handling: `entityUpdate` plus the derived events
    /// upstream extracts from metadata (sleep/crouch/elytra/item drop and the
    /// self-targeted firework).
    pub(super) fn metadata_update(
        &mut self,
        client: &mineflayer_engine::Client,
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

        // upstream fires entitySleep when the packet itself carries
        // `sleeping_pos` or `pose === 2` — metadata index 6 = pose (all
        // entities), index 14 = sleeping_pos (living entities)
        if packed_items.iter().any(|item| {
            (item.index == 6 && matches!(&item.value, EntityDataValue::Pose(Pose::Sleeping)))
                || (item.index == 14
                    && matches!(&item.value, EntityDataValue::OptionalBlockPos(Some(_))))
        }) {
            out.push(BotEvent::EntitySleep(entity.clone()));
        }

        // upstream gates crouch/uncrouch/elytra on the packet carrying
        // `shared_flags` (metadata index 0, byte): bit 0x80 is elytra, bit
        // 0x02 is sneaking. Uncrouch and the elytra edge use the last
        // flag-carried value, not the entity's current component state.
        if let Some(flags) = packed_items.iter().find_map(|item| {
            if item.index != 0 {
                return None;
            }
            match &item.value {
                EntityDataValue::Byte(flags) => Some(*flags),
                _ => None,
            }
        }) {
            let meta = self.meta.entry(id).or_default();
            let fall_flying = flags & 0x80 != 0;
            let shift_down = flags & 0x02 != 0;
            if fall_flying && !meta.fall_flying {
                out.push(BotEvent::EntityElytraFlew(entity.clone()));
            }
            if !fall_flying && meta.fall_flying && Some(id) == self_id {
                // upstream clears the firework boost when we stop elytra flying
                self.firework_rocket_duration = 0;
                self.seen_fireworks.clear();
            }
            if shift_down {
                out.push(BotEvent::EntityCrouch(entity.clone()));
            } else if meta.shift_key_down {
                out.push(BotEvent::EntityUncrouch(entity.clone()));
            }
            *meta = EntityMeta {
                shift_key_down: shift_down,
                fall_flying,
            };
        }

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
    pub(super) fn set_passengers(
        &mut self,
        client: &mineflayer_engine::Client,
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
pub(super) enum StatusKind {
    Hurt,
    Dead,
    Taming,
    Tamed,
    ShakingOffWater,
    EatingGrass,
    HandSwap,
}

pub(super) fn entity_status_kind(event_id: u8) -> Option<StatusKind> {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mineflayer_engine::protocol::packets::game::c_animate::ClientboundAnimate;

    use super::*;

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

    /// A dimension change keeps only the bot's own entries (A9).
    #[test]
    fn respawn_forgets_other_dimension_entities() {
        use mineflayer_engine::{
            core::game_type::{GameMode, OptionalGameType},
            protocol::packets::{
                common::CommonPlayerSpawnInfo, game::c_respawn::ClientboundRespawn,
            },
            registry::{DataRegistry as _, data::DimensionKind},
        };
        let me = MinecraftEntityId(7);
        let other = MinecraftEntityId(8);
        let client = crate::test_util::live_entity_client(me);
        let mut tracker = EntityTracker::default();
        for id in [me, other] {
            tracker.equipment.insert(id, HashMap::new());
            tracker.attributes.insert(id, HashMap::new());
            tracker.effects.insert(id, HashMap::new());
            tracker.meta.insert(id, EntityMeta::default());
        }
        tracker.vehicle_of.insert(other, me);
        tracker.passengers_of.insert(me, vec![other]);
        tracker.seen_fireworks.insert(other);
        tracker.difficulty = Some(Difficulty::Hard);

        let respawn = ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: CommonPlayerSpawnInfo {
                dimension_type: DimensionKind::new_raw(0),
                dimension: mineflayer_engine::Identifier::new("minecraft:the_nether"),
                seed: 0,
                game_type: GameMode::Survival,
                previous_game_type: OptionalGameType(None),
                is_debug: false,
                is_flat: false,
                last_death_location: None,
                portal_cooldown: 0,
                sea_level: 32,
            },
            data_to_keep: 0,
        });
        assert!(tracker.handle_packet(&client, &respawn).is_empty());

        for map_len in [
            tracker.equipment.len(),
            tracker.attributes.len(),
            tracker.effects.len(),
            tracker.meta.len(),
        ] {
            assert_eq!(map_len, 1, "only the bot's own entry stays");
        }
        assert!(tracker.equipment.contains_key(&me));
        assert!(tracker.vehicle_of.is_empty() && tracker.passengers_of.is_empty());
        assert!(tracker.seen_fireworks.is_empty());
        assert_eq!(tracker.difficulty, Some(Difficulty::Hard));
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

    #[test]
    fn crouch_and_sleep_require_the_metadata_item() {
        use mineflayer_engine::{
            BlockPos,
            entity::{EntityMetadataItems, metadata::AbstractEntityShiftKeyDown},
            protocol::packets::game::c_set_entity_data::ClientboundSetEntityData,
        };

        fn packet(items: Vec<EntityDataItem>) -> ClientboundGamePacket {
            ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
                id: MinecraftEntityId(7),
                packed_items: EntityMetadataItems(items),
            })
        }
        let flags = |b: u8| EntityDataItem {
            index: 0,
            value: EntityDataValue::Byte(b),
        };
        let air = EntityDataItem {
            index: 1,
            value: EntityDataValue::Int(300),
        };

        let client = crate::test_util::live_entity_client(());
        // the entity IS crouched component-side — the regression this test
        // pins is that packets without shared_flags still emit nothing
        let e = client
            .ecs
            .write()
            .spawn((MinecraftEntityId(7), AbstractEntityShiftKeyDown(true)))
            .id();
        let mut tracker = EntityTracker::default();
        tracker.known.insert(MinecraftEntityId(7), e);

        // unrelated metadata while crouched: EntityUpdate only, no Crouch
        let out = tracker.handle_packet(&client, &packet(vec![air.clone()]));
        assert!(
            matches!(&out[..], [BotEvent::EntityUpdate(_)]),
            "expected EntityUpdate only, got {out:?}"
        );
        // shared_flags 0x02: crouch fires, and refires per flag packet
        // (upstream emits unconditionally while the bit is set)
        for _ in 0..2 {
            let out = tracker.handle_packet(&client, &packet(vec![flags(0x02)]));
            assert!(
                matches!(
                    &out[..],
                    [BotEvent::EntityUpdate(_), BotEvent::EntityCrouch(_)]
                ),
                "expected Update+Crouch, got {out:?}"
            );
        }
        // still crouched component-side, but this packet carries no flags:
        // nothing crouch-related may emit
        let out = tracker.handle_packet(&client, &packet(vec![air]));
        assert!(
            matches!(&out[..], [BotEvent::EntityUpdate(_)]),
            "expected EntityUpdate only, got {out:?}"
        );
        // flags with the bit clear: the release edge fires once
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntityUncrouch(_)]
            ),
            "expected Update+Uncrouch, got {out:?}"
        );
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
        assert!(
            matches!(&out[..], [BotEvent::EntityUpdate(_)]),
            "expected EntityUpdate only, got {out:?}"
        );

        // sleep fires per packet carrying pose=Sleeping or a Some
        // sleeping_pos — never on unrelated metadata
        let sleeping_pose = EntityDataItem {
            index: 6,
            value: EntityDataValue::Pose(Pose::Sleeping),
        };
        let bed_pos = EntityDataItem {
            index: 14,
            value: EntityDataValue::OptionalBlockPos(Some(BlockPos::new(1, 64, 1))),
        };
        let no_bed = EntityDataItem {
            index: 14,
            value: EntityDataValue::OptionalBlockPos(None),
        };
        let out = tracker.handle_packet(&client, &packet(vec![sleeping_pose]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntitySleep(_)]
            ),
            "expected Update+Sleep, got {out:?}"
        );
        let out = tracker.handle_packet(&client, &packet(vec![bed_pos]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntitySleep(_)]
            ),
            "expected Update+Sleep, got {out:?}"
        );
        // an absent sleeping_pos (waking) carries no sleep signal
        let out = tracker.handle_packet(&client, &packet(vec![no_bed]));
        assert!(
            matches!(&out[..], [BotEvent::EntityUpdate(_)]),
            "expected EntityUpdate only, got {out:?}"
        );
    }

    #[test]
    fn elytra_flew_is_flag_packet_gated_and_edged() {
        use mineflayer_engine::{
            entity::EntityMetadataItems,
            protocol::packets::game::c_set_entity_data::ClientboundSetEntityData,
        };

        fn packet(items: Vec<EntityDataItem>) -> ClientboundGamePacket {
            ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
                id: MinecraftEntityId(7),
                packed_items: EntityMetadataItems(items),
            })
        }
        let flags = |b: u8| EntityDataItem {
            index: 0,
            value: EntityDataValue::Byte(b),
        };

        let client = crate::test_util::live_entity_client(());
        let e = client.ecs.write().spawn(MinecraftEntityId(7)).id();
        let mut tracker = EntityTracker::default();
        tracker.known.insert(MinecraftEntityId(7), e);

        // fall-flying bit: fires on the false->true edge only
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntityElytraFlew(_)]
            ),
            "expected Update+ElytraFlew, got {out:?}"
        );
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
        assert!(
            matches!(&out[..], [BotEvent::EntityUpdate(_)]),
            "expected EntityUpdate only, got {out:?}"
        );
        // bit clears then sets again: a fresh edge refires
        tracker.handle_packet(&client, &packet(vec![flags(0x00)]));
        let out = tracker.handle_packet(&client, &packet(vec![flags(0x80)]));
        assert!(
            matches!(
                &out[..],
                [BotEvent::EntityUpdate(_), BotEvent::EntityElytraFlew(_)]
            ),
            "expected Update+ElytraFlew, got {out:?}"
        );
    }

    #[test]
    fn profile_keys_tracked_from_player_info_update() {
        use mineflayer_engine::{
            auth::game_profile::GameProfile,
            protocol::packets::game::{
                c_player_info_remove::ClientboundPlayerInfoRemove,
                c_player_info_update::{
                    ActionEnumSet, ClientboundPlayerInfoUpdate, PlayerInfoEntry,
                },
                s_chat_session_update::{ProfilePublicKeyData, RemoteChatSessionData},
            },
        };

        let client = crate::test_util::despawned_client();
        let mut tracker = EntityTracker::default();
        let uuid = uuid::Uuid::from_u128(0x1234);
        tracker.handle_packet(
            &client,
            &ClientboundGamePacket::PlayerInfoUpdate(ClientboundPlayerInfoUpdate {
                actions: ActionEnumSet {
                    add_player: true,
                    initialize_chat: true,
                    update_game_mode: true,
                    update_listed: true,
                    update_latency: true,
                    update_display_name: false,
                    update_list_order: false,
                    update_hat: false,
                },
                entries: vec![PlayerInfoEntry {
                    profile: GameProfile::new(uuid, "faye".into()),
                    chat_session: Some(RemoteChatSessionData {
                        session_id: uuid::Uuid::from_u128(0xbeef),
                        profile_public_key: ProfilePublicKeyData {
                            expires_at: 0,
                            key: vec![1, 2, 3],
                            key_signature: vec![4, 5],
                        },
                    }),
                    ..Default::default()
                }],
            }),
        );
        assert_eq!(
            tracker.chat_sessions.get(&uuid),
            Some(&ProfileKeys {
                public_key: vec![1, 2, 3],
                signature: vec![4, 5],
            })
        );

        // a re-add without a chat session clears the old key
        tracker.handle_packet(
            &client,
            &ClientboundGamePacket::PlayerInfoUpdate(ClientboundPlayerInfoUpdate {
                actions: ActionEnumSet {
                    add_player: true,
                    initialize_chat: false,
                    update_game_mode: false,
                    update_listed: false,
                    update_latency: false,
                    update_display_name: false,
                    update_list_order: false,
                    update_hat: false,
                },
                entries: vec![PlayerInfoEntry {
                    profile: GameProfile::new(uuid, "faye".into()),
                    ..Default::default()
                }],
            }),
        );
        assert!(tracker.chat_sessions.is_empty());

        // re-add the key, then PlayerInfoRemove clears it
        tracker.chat_sessions.insert(
            uuid,
            ProfileKeys {
                public_key: vec![1],
                signature: vec![2],
            },
        );
        tracker.handle_packet(
            &client,
            &ClientboundGamePacket::PlayerInfoRemove(ClientboundPlayerInfoRemove {
                profile_ids: vec![uuid],
            }),
        );
        assert!(tracker.chat_sessions.is_empty());
    }
}
