//! The entity-event reducer: packet decode into [`EntityTracker`] state
//! and the entity `BotEvent`s the engine does not raise itself.

use super::*;
use mineflayer_engine::protocol::packets::game::{
    ClientboundAnimate, ClientboundDamageEvent, ClientboundEntityEvent,
    ClientboundPlayerInfoUpdate, ClientboundRemoveEntities, ClientboundRemoveMobEffect,
    ClientboundSetEntityLink, ClientboundSetEquipment, ClientboundUpdateAttributes,
    ClientboundUpdateMobEffect,
};

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
            RemoveEntities(p) => self.removed(client, p, self_id),
            MoveEntityPos(p) => self.moved(client, p.entity_id),
            MoveEntityPosRot(p) => self.moved(client, p.entity_id),
            MoveEntityRot(p) => self.moved(client, p.entity_id),
            TeleportEntity(p) => self.moved(client, p.id),
            EntityPositionSync(p) => self.moved(client, p.id),
            RotateHead(p) => self.moved(client, p.entity_id),
            SetEntityData(p) => self.metadata_update(client, p.id, &p.packed_items.0, self_id),
            Animate(p) => self.animated(client, p),
            EntityEvent(p) => self.status(client, p),
            DamageEvent(p) => self.damaged(client, p),
            SetEquipment(p) => self.equipped(client, p),
            UpdateAttributes(p) => self.attributes_updated(client, p),
            UpdateMobEffect(p) => self.effect_added(client, p),
            RemoveMobEffect(p) => self.effect_removed(client, p),
            TakeItemEntity(p) => vec![BotEvent::PlayerCollect {
                collector: self.resolve(client, p.player_id),
                collected: self.resolve(client, MinecraftEntityId::from(p.item_id)),
            }],
            SetEntityLink(p) => self.linked(client, p),
            SetPassengers(p) => self.set_passengers(client, p.vehicle, &p.passengers, self_id),
            ChangeDifficulty(p) => {
                self.difficulty = Some(p.difficulty);
                vec![]
            }
            PlayerInfoUpdate(p) => self.player_info_updated(p),
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

    fn removed(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundRemoveEntities,
        self_id: Option<MinecraftEntityId>,
    ) -> Vec<BotEvent> {
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

    fn animated(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundAnimate,
    ) -> Vec<BotEvent> {
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

    fn status(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundEntityEvent,
    ) -> Vec<BotEvent> {
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

    fn damaged(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundDamageEvent,
    ) -> Vec<BotEvent> {
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

    fn equipped(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundSetEquipment,
    ) -> Vec<BotEvent> {
        let slots = self.equipment.entry(p.entity_id).or_default();
        for (slot, item) in &p.slots.slots {
            slots.insert(*slot, item.clone());
        }
        self.resolve(client, p.entity_id)
            .map(BotEvent::EntityEquip)
            .into_iter()
            .collect()
    }

    fn attributes_updated(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundUpdateAttributes,
    ) -> Vec<BotEvent> {
        let attributes = self.attributes.entry(p.entity_id).or_default();
        for snapshot in &p.values {
            attributes.insert(snapshot.attribute, snapshot.clone());
        }
        self.resolve(client, p.entity_id)
            .map(BotEvent::EntityAttributes)
            .into_iter()
            .collect()
    }

    fn effect_added(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundUpdateMobEffect,
    ) -> Vec<BotEvent> {
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

    fn effect_removed(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundRemoveMobEffect,
    ) -> Vec<BotEvent> {
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

    fn linked(
        &mut self,
        client: &mineflayer_engine::Client,
        p: &ClientboundSetEntityLink,
    ) -> Vec<BotEvent> {
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

    fn player_info_updated(&mut self, p: &ClientboundPlayerInfoUpdate) -> Vec<BotEvent> {
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
mod tests;
