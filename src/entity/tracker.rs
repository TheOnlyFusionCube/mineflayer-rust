//! The entity-event reducer: packet decode into per-entity components and
//! the entity `BotEvent`s the engine does not raise itself.
//!
//! The bridge decodes each packet after the engine applied it. For the
//! packets whose apply destroys what an event needs (a removed entity, an
//! ended effect, the metadata flags before a change), a `PacketPreApply`
//! hook records that state first ([`EntityPre`]).

use std::collections::VecDeque;

use super::*;
use crate::bot::{with_component_mut, with_entity_component_mut};
use mineflayer_engine::{
    ecs::{
        self as bevy_ecs,
        prelude::{Component, Mut, Resource},
        system::Commands,
        world::{CommandQueue, World},
    },
    entity::metadata::{AbstractEntityShiftKeyDown, SleepingPos, apply_metadata},
    protocol::packets::game::{
        ClientboundAnimate, ClientboundDamageEvent, ClientboundEntityEvent,
        ClientboundPlayerInfoUpdate, ClientboundRemoveEntities, ClientboundRemoveMobEffect,
        ClientboundSetEntityLink, ClientboundSetEquipment, ClientboundUpdateAttributes,
        ClientboundUpdateMobEffect,
    },
};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// `entity.equipment`, from `c_set_equipment`; the engine drops that
/// packet.
#[derive(Component, Default)]
pub(crate) struct Equipment(pub HashMap<EquipmentSlot, ItemStack>);

/// The raw `c_update_attributes` snapshots; the engine keeps only the
/// attributes it uses (no armor), and [`Bot::get_explosion_damages`] needs
/// armor.
#[derive(Component, Default)]
pub(crate) struct AttributeSnapshots(pub HashMap<Attribute, AttributeSnapshot>);

/// The entity this entity rides (`c_set_passengers`) or is leashed to
/// (`c_set_entity_link`).
#[derive(Component, Default)]
pub(crate) struct Vehicle(pub Option<MinecraftEntityId>);

/// A vehicle's passengers from the last `c_set_passengers`, for the
/// mount and dismount diff.
#[derive(Component, Default)]
struct Passengers(Vec<MinecraftEntityId>);

/// A firework rocket that `usedFirework` has counted.
#[derive(Component, Default)]
struct FireworkCounted;

/// The entity-domain state that belongs to the bot, not to one entity.
/// Upstream resets it on login.
#[derive(Component, Default)]
pub(crate) struct EntityDomain {
    /// `bot.fireworkRocketDuration`.
    pub firework_rocket_duration: u32,
    /// Chat-signing keys per player uuid, from `c_player_info_update`'s
    /// `initialize_chat` action; the engine's [`PlayerInfo`] drops them
    /// (`profileKeys` upstream).
    pub chat_sessions: HashMap<uuid::Uuid, ProfileKeys>,
}

// ---------------------------------------------------------------------------
// Pre-apply records
// ---------------------------------------------------------------------------

/// What an entity packet is about to destroy, read before the engine
/// applies it.
enum EntityPre {
    /// `c_remove_entities`: the ECS entity of each id. Packets earlier in
    /// the same pass resolve these ids too.
    Removed(Vec<(MinecraftEntityId, Entity)>),
    /// `c_remove_mob_effect`: the data of the effect that ends.
    EffectEnded(Option<MobEffectData>),
    /// `c_set_entity_data`, decoded through the engine's typed metadata.
    Metadata(MetadataChange),
}

/// The metadata fields one packet carries that the entity events need.
#[derive(Default)]
struct MetadataChange {
    /// `(before, after)` of the shift-key and the fall-flying flags, when
    /// the packet carries the shared flags.
    shift_key_down: Option<(bool, bool)>,
    fall_flying: Option<(bool, bool)>,
    /// The packet carries `pose = Sleeping` or a sleeping position.
    sleeping: bool,
}

/// The [`EntityPre`] records the bridge has not decoded yet, in packet
/// order, with each packet's address in its `Arc`.
#[derive(Component, Default)]
pub(crate) struct EntityPreApply(VecDeque<(usize, EntityPre)>);

/// A world to decode metadata into: the engine's metadata decode writes
/// typed components on an entity, so the hook decodes each packet on an
/// empty entity here and reads what the packet carries.
#[derive(Resource, Default)]
pub(crate) struct MetadataScratch(World);

/// The `PacketPreApply` hook of [`super::EntityStatePlugin`].
pub(crate) fn record_entity_pre(world: &mut World, client: Entity, packet: &ClientboundGamePacket) {
    let index = |world: &World, id| {
        world
            .get::<EntityIdIndex>(client)
            .and_then(|index| index.get_by_minecraft_entity(id))
    };
    let pre = match packet {
        ClientboundGamePacket::RemoveEntities(p) => EntityPre::Removed(
            p.entity_ids
                .iter()
                .filter_map(|&id| Some((id, index(world, id)?)))
                .collect(),
        ),
        ClientboundGamePacket::RemoveMobEffect(p) => EntityPre::EffectEnded(
            index(world, p.entity_id)
                .and_then(|e| world.get::<ActiveEffects>(e))
                .and_then(|effects| effects.get(p.effect).cloned()),
        ),
        ClientboundGamePacket::SetEntityData(p) => {
            let Some(entity) = index(world, p.id) else {
                return;
            };
            let Some(kind) = world.get::<EntityKindComponent>(entity).map(|k| k.0) else {
                return;
            };
            let before = (
                world
                    .get::<AbstractEntityShiftKeyDown>(entity)
                    .is_some_and(|c| c.0),
                world.get::<FallFlying>(entity).is_some_and(|c| c.0),
            );
            let change = world.resource_scope(|_, mut scratch: Mut<MetadataScratch>| {
                decode_metadata(&mut scratch.0, kind, &p.packed_items.0, before)
            });
            EntityPre::Metadata(change)
        }
        _ => return,
    };
    let entry = (std::ptr::from_ref(packet) as usize, pre);
    match world.get_mut::<EntityPreApply>(client) {
        Some(mut queue) => queue.0.push_back(entry),
        None => {
            world
                .entity_mut(client)
                .insert(EntityPreApply(VecDeque::from([entry])));
        }
    }
}

/// The fields of `items` that the entity events need, through the engine's
/// metadata decode for `kind`.
fn decode_metadata(
    scratch: &mut World,
    kind: EntityKind,
    items: &[EntityDataItem],
    (shift_before, fall_before): (bool, bool),
) -> MetadataChange {
    let entity = scratch.spawn_empty().id();
    let mut queue = CommandQueue::default();
    // a field of the wrong type fails the engine's apply too; the fields
    // before it still apply, as they do on the real entity
    let _ = apply_metadata(
        &mut Commands::new(&mut queue, scratch).entity(entity),
        kind,
        items.to_vec(),
    );
    queue.apply(scratch);
    let decoded = scratch.entity(entity);
    let change = MetadataChange {
        shift_key_down: decoded
            .get::<AbstractEntityShiftKeyDown>()
            .map(|c| (shift_before, c.0)),
        fall_flying: decoded.get::<FallFlying>().map(|c| (fall_before, c.0)),
        sleeping: decoded.get::<Pose>() == Some(&Pose::Sleeping)
            || decoded.get::<SleepingPos>().is_some_and(|p| p.0.is_some()),
    };
    scratch.despawn(entity);
    change
}

/// The record of `packet`; records of packets the bridge never decoded
/// are dropped on the way.
fn take_pre(
    client: &mineflayer_engine::Client,
    packet: &ClientboundGamePacket,
) -> Option<EntityPre> {
    let address = std::ptr::from_ref(packet) as usize;
    with_component_mut::<EntityPreApply, _>(client, |queue| {
        while let Some((at, pre)) = queue.0.pop_front() {
            if at == address {
                return Some(pre);
            }
        }
        None
    })
    .flatten()
}

// ---------------------------------------------------------------------------
// Event bridging (bridge.rs calls `observe_event` for every engine `Event`)
// ---------------------------------------------------------------------------

/// Feed one engine [`Event`] into the entity domain, returning the
/// [`BotEvent`]s it produced, right after the corresponding raw
/// `BotEvent::Packet`/`Spawn`.
pub(crate) fn observe_event(client: &mineflayer_engine::Client, event: &Event) -> Vec<BotEvent> {
    match event {
        Event::Packet(packet) => handle_packet(client, packet.as_ref()),
        // `bot.on('spawn', () => bot.emit('entitySpawn', bot.entity))` upstream
        Event::Spawn => vec![BotEvent::EntitySpawn(client.entity())],
        // upstream resets `bot.entities` on login; the per-entity state
        // goes with the entities
        Event::Login => {
            with_component_mut::<EntityDomain, _>(client, |d| *d = EntityDomain::default());
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

/// The ECS entity of `id`: the live index, else a removal later in this
/// pass that the bridge has not decoded yet.
fn resolve_id(client: &mineflayer_engine::Client, id: MinecraftEntityId) -> Option<Entity> {
    entity_id_by_minecraft_id(client, id).or_else(|| {
        crate::bot::with_component::<EntityPreApply, _>(client, |queue| {
            queue.0.iter().find_map(|(_, pre)| match pre {
                EntityPre::Removed(removed) => removed
                    .iter()
                    .find(|(gone, _)| *gone == id)
                    .map(|(_, e)| *e),
                _ => None,
            })
        })
        .flatten()
    })
}

/// An [`EntityRef`] for `id`, also for an entity removed later in this
/// pass (its handle then reads no components, like upstream's dead
/// entity object).
fn resolve(client: &mineflayer_engine::Client, id: MinecraftEntityId) -> Option<EntityRef> {
    resolve_id(client, id).map(|e| client.entity_ref_for(e))
}

/// `f` on the `T` of the entity with `id`, created on first use.
fn update<T: Component<Mutability = mineflayer_engine::ecs::component::Mutable> + Default, R>(
    client: &mineflayer_engine::Client,
    id: MinecraftEntityId,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    with_entity_component_mut(client, resolve_id(client, id)?, f)
}

/// Decode one clientbound packet into entity-domain [`BotEvent`]s. Runs
/// after the engine's own packet handler, so the ECS already reflects the
/// packet (indexes are populated for spawns, cleared for removals).
fn handle_packet(
    client: &mineflayer_engine::Client,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    use ClientboundGamePacket::*;
    let self_id = client.get_component::<MinecraftEntityId>().map(|id| *id);
    let one = |id| resolve(client, id).into_iter();
    match packet {
        AddEntity(p) => one(p.id).map(BotEvent::EntitySpawn).collect(),
        RemoveEntities(p) => removed(client, p, take_pre(client, packet), self_id),
        MoveEntityPos(p) => one(p.entity_id).map(BotEvent::EntityMoved).collect(),
        MoveEntityPosRot(p) => one(p.entity_id).map(BotEvent::EntityMoved).collect(),
        MoveEntityRot(p) => one(p.entity_id).map(BotEvent::EntityMoved).collect(),
        TeleportEntity(p) => one(p.id).map(BotEvent::EntityMoved).collect(),
        EntityPositionSync(p) => one(p.id).map(BotEvent::EntityMoved).collect(),
        RotateHead(p) => one(p.entity_id).map(BotEvent::EntityMoved).collect(),
        SetEntityData(p) => {
            let change = match take_pre(client, packet) {
                Some(EntityPre::Metadata(change)) => change,
                _ => MetadataChange::default(),
            };
            metadata_update(client, p.id, &p.packed_items.0, change, self_id)
        }
        Animate(p) => animated(client, p),
        EntityEvent(p) => status(client, p),
        DamageEvent(p) => damaged(client, p),
        SetEquipment(p) => equipped(client, p),
        UpdateAttributes(p) => attributes_updated(client, p),
        UpdateMobEffect(p) => effect_added(client, p),
        RemoveMobEffect(p) => effect_removed(client, p, take_pre(client, packet)),
        TakeItemEntity(p) => vec![BotEvent::PlayerCollect {
            collector: resolve(client, p.player_id),
            collected: resolve(client, MinecraftEntityId::from(p.item_id)),
        }],
        SetEntityLink(p) => linked(client, p),
        SetPassengers(p) => set_passengers(client, p.vehicle, &p.passengers, self_id),
        PlayerInfoUpdate(p) => {
            with_component_mut::<EntityDomain, _>(client, |d| player_info_updated(d, p));
            vec![]
        }
        PlayerInfoRemove(p) => {
            with_component_mut::<EntityDomain, _>(client, |d| {
                for uuid in &p.profile_ids {
                    d.chat_sessions.remove(uuid);
                }
            });
            vec![]
        }
        _ => vec![],
    }
}

fn removed(
    client: &mineflayer_engine::Client,
    p: &ClientboundRemoveEntities,
    pre: Option<EntityPre>,
    self_id: Option<MinecraftEntityId>,
) -> Vec<BotEvent> {
    let removed = match pre {
        Some(EntityPre::Removed(removed)) => removed,
        _ => Vec::new(),
    };
    let own_vehicle = with_component_mut::<Vehicle, _>(client, |v| v.0).flatten();
    let mut out = Vec::new();
    for &id in &p.entity_ids {
        // EntityGone and (if it was our vehicle) Dismounted carry the
        // stale handle, like upstream's dead entity object
        let entity = removed
            .iter()
            .find(|(gone, _)| *gone == id)
            .map(|(_, e)| client.entity_ref_for(*e));
        out.push(BotEvent::EntityGone {
            entity: entity.clone(),
            id,
        });
        if own_vehicle == Some(id) && self_id != Some(id) {
            with_component_mut::<Vehicle, _>(client, |v| v.0 = None);
            out.push(BotEvent::Dismounted { vehicle: entity });
        }
    }
    out
}

fn animated(client: &mineflayer_engine::Client, p: &ClientboundAnimate) -> Vec<BotEvent> {
    let Some(entity) = resolve(client, p.id) else {
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

fn status(client: &mineflayer_engine::Client, p: &ClientboundEntityEvent) -> Vec<BotEvent> {
    let Some(entity) = resolve(client, p.entity_id) else {
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
            update::<Equipment, _>(client, p.entity_id, |Equipment(equipment)| {
                let main = equipment.remove(&EquipmentSlot::Mainhand);
                let off = equipment.remove(&EquipmentSlot::Offhand);
                if let Some(item) = off {
                    equipment.insert(EquipmentSlot::Mainhand, item);
                }
                if let Some(item) = main {
                    equipment.insert(EquipmentSlot::Offhand, item);
                }
            });
            vec![BotEvent::EntityHandSwap(entity)]
        }
        None => vec![],
    }
}

fn damaged(client: &mineflayer_engine::Client, p: &ClientboundDamageEvent) -> Vec<BotEvent> {
    let Some(entity) = resolve(client, p.entity_id) else {
        return vec![];
    };
    let source = p
        .source_cause_id
        .0
        .map(MinecraftEntityId::from)
        .and_then(|id| resolve(client, id));
    vec![BotEvent::EntityHurt { entity, source }]
}

fn equipped(client: &mineflayer_engine::Client, p: &ClientboundSetEquipment) -> Vec<BotEvent> {
    update::<Equipment, _>(client, p.entity_id, |Equipment(slots)| {
        for (slot, item) in &p.slots.slots {
            slots.insert(*slot, item.clone());
        }
    });
    resolve(client, p.entity_id)
        .map(BotEvent::EntityEquip)
        .into_iter()
        .collect()
}

fn attributes_updated(
    client: &mineflayer_engine::Client,
    p: &ClientboundUpdateAttributes,
) -> Vec<BotEvent> {
    update::<AttributeSnapshots, _>(client, p.entity_id, |AttributeSnapshots(attributes)| {
        for snapshot in &p.values {
            attributes.insert(snapshot.attribute, snapshot.clone());
        }
    });
    resolve(client, p.entity_id)
        .map(BotEvent::EntityAttributes)
        .into_iter()
        .collect()
}

fn effect_added(
    client: &mineflayer_engine::Client,
    p: &ClientboundUpdateMobEffect,
) -> Vec<BotEvent> {
    resolve(client, p.entity_id)
        .map(|entity| BotEvent::EntityEffect {
            entity,
            effect: p.mob_effect,
            data: p.data.clone(),
        })
        .into_iter()
        .collect()
}

fn effect_removed(
    client: &mineflayer_engine::Client,
    p: &ClientboundRemoveMobEffect,
    pre: Option<EntityPre>,
) -> Vec<BotEvent> {
    // upstream emits the tracked effect object and falls back to
    // `{id, amplifier: -1, duration: -1}` when the effect was never seen —
    // the remove packet carries only the kind
    let data = match pre {
        Some(EntityPre::EffectEnded(Some(data))) => data,
        _ => MobEffectData {
            amplifier: -1,
            duration: -1,
            ..Default::default()
        },
    };
    resolve(client, p.entity_id)
        .map(|entity| BotEvent::EntityEffectEnd {
            entity,
            effect: p.effect,
            data,
        })
        .into_iter()
        .collect()
}

fn linked(client: &mineflayer_engine::Client, p: &ClientboundSetEntityLink) -> Vec<BotEvent> {
    let Some(entity) = resolve(client, p.source_id) else {
        return vec![];
    };
    if p.dest_id == MinecraftEntityId(-1) {
        let vehicle = update::<Vehicle, _>(client, p.source_id, |v| v.0.take())
            .flatten()
            .and_then(|id| resolve(client, id));
        vec![BotEvent::EntityDetach { entity, vehicle }]
    } else {
        update::<Vehicle, _>(client, p.source_id, |v| v.0 = Some(p.dest_id));
        let vehicle = resolve(client, p.dest_id);
        vec![BotEvent::EntityAttach { entity, vehicle }]
    }
}

fn player_info_updated(domain: &mut EntityDomain, p: &ClientboundPlayerInfoUpdate) {
    for entry in &p.entries {
        // upstream rebuilds the player object on add_player — a re-add
        // without chat keys clears them here too
        if p.actions.add_player {
            domain.chat_sessions.remove(&entry.profile.uuid);
        }
        if let Some(session) = &entry.chat_session {
            domain.chat_sessions.insert(
                entry.profile.uuid,
                ProfileKeys {
                    public_key: session.profile_public_key.key.clone(),
                    signature: session.profile_public_key.key_signature.clone(),
                },
            );
        }
    }
}

/// `entity_metadata` handling: `entityUpdate` plus the derived events
/// upstream extracts from metadata (sleep/crouch/elytra/item drop and the
/// self-targeted firework).
fn metadata_update(
    client: &mineflayer_engine::Client,
    id: MinecraftEntityId,
    packed_items: &[EntityDataItem],
    change: MetadataChange,
    self_id: Option<MinecraftEntityId>,
) -> Vec<BotEvent> {
    let Some(entity) = resolve(client, id) else {
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
    // `sleeping_pos` or `pose === 2`
    if change.sleeping {
        out.push(BotEvent::EntitySleep(entity.clone()));
    }

    // upstream gates crouch/uncrouch/elytra on the packet carrying
    // `shared_flags`. Uncrouch and the elytra edge compare with the last
    // flag-carried value, which is the engine component before the apply.
    if let Some((was_flying, flying)) = change.fall_flying {
        if flying && !was_flying {
            out.push(BotEvent::EntityElytraFlew(entity.clone()));
        }
        if !flying && was_flying && Some(id) == self_id {
            // upstream clears the firework boost when we stop elytra flying
            with_component_mut::<EntityDomain, _>(client, |d| d.firework_rocket_duration = 0);
        }
    }
    if let Some((was_down, down)) = change.shift_key_down {
        if down {
            out.push(BotEvent::EntityCrouch(entity.clone()));
        } else if was_down {
            out.push(BotEvent::EntityUncrouch(entity.clone()));
        }
    }

    // firework rockets attached to us feed `usedFirework` +
    // `bot.fireworkRocketDuration` (upstream reads the metadata the same
    // way); a rocket is looked at once, on its first metadata packet
    if entity
        .get_component::<EntityKindComponent>()
        .map(|kind| kind.0)
        == Some(EntityKind::FireworkRocket)
        && entity.get_component::<FireworkCounted>().is_none()
        && with_entity_component_mut::<FireworkCounted, _>(client, entity.id(), |_| ()).is_some()
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
        let jitter =
            (id.0 as u32).wrapping_mul(2654435761) % 6 + (id.0 as u32).wrapping_mul(2246822519) % 7;
        with_component_mut::<EntityDomain, _>(client, |d| {
            d.firework_rocket_duration = (10 * (flight + 1)) as u32 + jitter;
        });
        out.push(BotEvent::UsedFirework(id));
    }

    out
}

/// `c_set_passengers` — update vehicle links and emit `mount`/`dismount`
/// when our own ride changes (upstream semantics; `entityAttach` is only
/// for leashes, see `SetEntityLink`).
fn set_passengers(
    client: &mineflayer_engine::Client,
    vehicle: MinecraftEntityId,
    passengers: &[MinecraftEntityId],
    self_id: Option<MinecraftEntityId>,
) -> Vec<BotEvent> {
    let was = update::<Passengers, _>(client, vehicle, |p| {
        std::mem::replace(&mut p.0, passengers.to_vec())
    })
    .unwrap_or_default();
    let mut out = Vec::new();
    for &id in passengers.iter().filter(|p| !was.contains(p)) {
        update::<Vehicle, _>(client, id, |v| v.0 = Some(vehicle));
        if Some(id) == self_id {
            out.push(BotEvent::Mounted {
                vehicle: resolve(client, vehicle),
            });
        }
    }
    for &id in was.iter().filter(|p| !passengers.contains(p)) {
        update::<Vehicle, _>(client, id, |v| v.0 = None);
        if Some(id) == self_id {
            out.push(BotEvent::Dismounted {
                vehicle: resolve(client, vehicle),
            });
        }
    }
    out
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
