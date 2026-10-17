//! Entities, players, and combat — queries, the entity event family, and the
//! attack/interact/mount action surface.
//!
//! Entity *state* lives in the engine's ECS (tracked by its `EntityPlugin`); this
//! module only adds what the engine doesn't expose: packet-driven entity events
//! ([`BotEvent`] variants decoded in `observe_event`), tracked side state
//! that isn't in the ECS (equipment, raw attributes, vehicles), and the
//! `bot.*` method surface.

use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use parking_lot::RwLock;

use mineflayer_engine::{
    EntityRef, Event, FormattedText, Vec3,
    core::{difficulty::Difficulty, direction::Direction, entity_id::MinecraftEntityId},
    ecs::{
        entity::Entity,
        query::{QueryFilter, With},
    },
    entity::{
        ActiveEffects, EntityDataItem, EntityDataValue, EntityKindComponent, LookDirection,
        MobEffectData, Physics, Pose, Position,
        indexing::EntityIdIndex,
        metadata::{
            AttachedToTarget, FallFlying, FireworksItem, PaintingDirection, PaintingVariant,
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
    world::WorldName,
};
use regex::Regex;

use crate::{Bot, BotEvent, Error, Result};

mod damage;
mod tracker;

pub(crate) use tracker::{
    EntityTracker, entity_by_minecraft_id, entity_id_by_minecraft_id, observe_event,
};

// ---------------------------------------------------------------------------
// Queries and properties
// ---------------------------------------------------------------------------

/// The mineflayer `Player` object — a tab-list entry plus its world entity.
///
/// `info` is the engine's [`PlayerInfo`] (`profile.uuid`, `gamemode`, `latency`,
/// `display_name`); `entity` is the player's in-world entity when it's
/// spawned and loaded. `profile_keys` is the chat-signing key upstream
/// exposes as `player.profileKeys` — the engine's `PlayerInfo` drops it, so the
/// bridge tracks it off the `c_player_info_update` wire data.
#[derive(Clone)]
pub struct Player {
    /// The full tab-list entry.
    pub info: PlayerInfo,
    /// The player entity in the world, if spawned and loaded.
    pub entity: Option<EntityRef>,
    /// `player.profileKeys` — the chat-signing public key the server sent
    /// for this player, if any.
    pub profile_keys: Option<ProfileKeys>,
}

/// `player.profileKeys` — the player's chat-signing public key and Mojang
/// signature, decoded from the `initialize_chat` action of
/// `c_player_info_update` (`RemoteChatSessionData.profile_public_key`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileKeys {
    /// `profileKeys.publicKey` — the player's RSA public key (DER bytes).
    pub public_key: Vec<u8>,
    /// `profileKeys.signature` — Mojang's signature over the key.
    pub signature: Vec<u8>,
}

/// `Painting` (module export) — upstream's `{id, position, name, direction}`
/// wrapper, built from a painting entity's tracked metadata.
///
/// Upstream constructs it from the `spawn_entity_painting` packet, which no
/// longer exists in mc26 — paintings arrive as regular entities
/// (`EntityKind::Painting`) carrying `PaintingVariant`/`PaintingDirection`
/// metadata. [`Painting::from_entity`] reads those components off any
/// [`EntityRef`]; upstream's `block.painting`/paintings-by-position index
/// isn't ported (enumerate `bot.entities()` and filter through
/// `from_entity` instead).
#[derive(Clone, Debug)]
pub struct Painting {
    /// `painting.id` — the protocol entity id.
    pub id: MinecraftEntityId,
    /// `painting.position`.
    pub position: Vec3,
    /// `painting.name` — the variant as its registry handle. Resolve the
    /// title (`"kebab"`, `"pigscene"`, …) with
    /// `mineflayer_engine::Client::resolve_registry_key` →
    /// `data::PaintingVariantKey`.
    pub variant: mineflayer_engine::registry::data::PaintingVariant,
    /// `painting.direction` — the wall face the painting hangs on
    /// (upstream stores the face's normal `Vec3`; the engine decodes the typed
    /// [`Direction`]).
    pub direction: Direction,
}

impl Painting {
    /// Build from a painting [`EntityRef`]; `None` for any other entity
    /// kind (or an unloaded one).
    pub fn from_entity(entity: &EntityRef) -> Option<Painting> {
        // `_ =` drops the lock guard at the statement (a `let`-binding
        // would hold it for the rest of the function).
        let _ = entity.get_component::<mineflayer_engine::entity::metadata::Painting>()?;
        Some(Painting {
            id: *entity.get_component::<MinecraftEntityId>()?,
            position: **entity.get_component::<Position>()?,
            variant: entity.get_component::<PaintingVariant>()?.0,
            direction: entity.get_component::<PaintingDirection>()?.0,
        })
    }
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
    pub fn gamemode(&self) -> mineflayer_engine::core::game_type::GameMode {
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
    /// `bot.entity` — the bot's own entity as an engine [`mineflayer_engine::EntityRef`],
    /// usable anywhere a world entity is expected.
    pub fn entity(&self) -> mineflayer_engine::EntityRef {
        self.client.entity()
    }

    /// `bot.entity.position` convenience — the bot's feet position.
    /// `Vec3::ZERO` until the first position arrives (the engine's own default).
    pub fn position(&self) -> mineflayer_engine::Vec3 {
        self.client
            .get_component::<Position>()
            .map(|p| **p)
            .unwrap_or_default()
    }

    /// Entities in our world, sorted nearest-first — the listing
    /// `nearest_entities_by` produces, but under **one** ECS lock with the
    /// `EntityRef`s built only after it drops. The engine's
    /// `nearest_entity_ids_by` binds its `WorldName` component guard
    /// across the `Position` lookup — a nested `ecs.read()` that
    /// permanently deadlocks whenever the ECS runner queues a write
    /// between the two reads (parking_lot blocks a second shared lock
    /// behind a parked writer, and the first guard belongs to the same
    /// thread).
    fn entities_sorted_by_distance<F: QueryFilter>(&self) -> Vec<EntityRef> {
        let client = &self.client;
        let entities = {
            let mut ecs = client.ecs.write();
            let Ok(me) = ecs.get_entity(client.entity) else {
                return Vec::new();
            };
            let Some(world_name) = me.get::<WorldName>().cloned() else {
                return Vec::new();
            };
            let Some(position) = me.get::<Position>().map(|p| **p) else {
                return Vec::new();
            };
            let mut query = ecs.query_filtered::<(Entity, &WorldName, &Position), F>();
            let mut entities: Vec<(Entity, Vec3)> = query
                .iter(&ecs)
                .filter(|(_, e_world_name, _)| *e_world_name == &world_name)
                .map(|(e, _, pos)| (e, **pos))
                .collect();
            entities.sort_by_cached_key(|(_, pos)| pos.distance_squared_to(position).to_bits());
            entities
        };
        entities
            .into_iter()
            .map(|(e, _)| client.entity_ref_for(e))
            .collect()
    }

    /// `bot.entities` — every entity in view, keyed by protocol entity id
    /// (like upstream's `{[id]: Entity}`), including the bot itself.
    pub fn entities(&self) -> HashMap<MinecraftEntityId, EntityRef> {
        self.entities_sorted_by_distance::<With<Position>>()
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
        self.entities_sorted_by_distance::<With<Position>>()
            .into_iter()
            .find(|e| e.id() != own && matcher(e))
    }

    /// `bot.findPlayer`/`bot.findPlayers` — player entities matching
    /// `filter` (upstream returns entities, not tab-list entries).
    pub fn find_players(&self, filter: PlayerFilter) -> Vec<EntityRef> {
        self.entities_sorted_by_distance::<With<mineflayer_engine::entity::metadata::Player>>()
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
        // (lock rule, bot/mod.rs: copy out of the ECS guard and drop it,
        // then take the next lock.)
        let infos: Vec<PlayerInfo> = self
            .client
            .get_component::<mineflayer_engine::local_player::TabList>()
            .map(|tab_list| tab_list.values().cloned().collect())
            .unwrap_or_default();
        infos
            .into_iter()
            .map(|info| {
                let entity = self.client.entity_by_uuid(info.uuid);
                let profile_keys = self
                    .shared
                    .entities
                    .read()
                    .chat_sessions
                    .get(&info.uuid)
                    .cloned();
                (
                    info.profile.name.clone(),
                    Player {
                        info,
                        entity,
                        profile_keys,
                    },
                )
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
        self.player_by_uuid(uuid)
    }

    /// `bot._playerFromUUID` — tab-list lookup by UUID. Upstream takes the
    /// hyphenated string; parse one with `Uuid::parse_str`.
    pub fn player_by_uuid(&self, uuid: uuid::Uuid) -> Option<Player> {
        self.players().into_values().find(|p| p.info.uuid == uuid)
    }

    /// `bot.uuidToUsername` — `{[uuid]: username}` over the tab list. Keys
    /// are hyphenated UUID strings, like upstream.
    pub fn uuid_to_username(&self) -> HashMap<String, String> {
        self.client
            .get_component::<mineflayer_engine::local_player::TabList>()
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
        let vehicle = *self.shared.entities.read().vehicle_of.get(&id)?;
        self.shared.entities.write().resolve(&self.client, vehicle)
    }

    /// `entity.equipment` — the entity's equipment by slot, tracked from
    /// `c_set_equipment` (the engine drops that packet). Empty for entities that
    /// never sent equipment.
    pub fn equipment(&self, entity: &EntityRef) -> HashMap<EquipmentSlot, ItemStack> {
        // resolve the id before touching the tracker — holding the
        // component guard across the tracker read inverts the bridge's
        // (tracker-write → ecs-read) lock order and deadlocks
        let id = entity.get_component::<MinecraftEntityId>().map(|id| *id);
        id.and_then(|id| self.shared.entities.read().equipment.get(&id).cloned())
            .unwrap_or_default()
    }

    /// `bot.fireworkRocketDuration` — estimated remaining boost ticks from
    /// the last `usedFirework`; `0` when none is active.
    pub fn firework_rocket_duration(&self) -> u32 {
        self.shared.entities.read().firework_rocket_duration
    }

    // -- actions ---------------------------------------------------------

    /// `bot.attack(entity, swing?)` — with `swing` it queues the attack
    /// through the engine (which sends attack + arm swing on the next game tick,
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
    /// talk to villager / board minecart). Looks at `entity.position + 1`
    /// first, like upstream's `lookAt(entity.position.offset(0, 1, 0))`.
    /// Errors on a positionless entity like upstream's throw on
    /// `entity.position.offset` — and, like upstream, sends nothing.
    pub async fn activate_entity(&self, entity: &EntityRef) -> Result<()> {
        let pos = entity
            .get_component::<Position>()
            .map(|p| **p)
            .ok_or_else(|| Error::NotFound("entity has no position".to_string()))?;
        self.look_at(pos.up(1.), false).await?;
        self.client.entity_interact(entity.id());
        Ok(())
    }

    /// `bot.activateEntityAt(entity, pos)` — right-click at a specific point
    /// on the entity's hitbox (e.g. armor stands). Looks at `pos` first,
    /// like upstream's `lookAt(position)`.
    pub async fn activate_entity_at(&self, entity: &EntityRef, pos: Vec3) -> Result<()> {
        self.look_at(pos, false).await?;
        self.client
            .ecs
            .write()
            .trigger(mineflayer_engine::interact::EntityInteractEvent {
                client: self.client.entity,
                target: entity.id(),
                location: Some(pos),
            });
        Ok(())
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
        // The engine sends player_input diffs against its own LastSentInput, so a
        // matching release restores the on-wire state without a second packet
        self.client.write_packet(ServerboundPlayerInput::default());
        // edge: if the engine already thinks shift is held (trying_to_crouch),
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
    /// levitation). Upstream's elytra-equipped check is intentionally
    /// absent; the server rejects the packet without one.
    pub fn elytra_fly(&self) -> Result<()> {
        let me = self.entity();
        if me
            .get_component::<FallFlying>()
            .map(|f| f.0)
            .unwrap_or(false)
        {
            return Err(Error::InvalidState("already elytra flying".to_string()));
        }
        let physics = me.get_component::<Physics>().ok_or(Error::NotConnected)?;
        // extract before the later get_component calls — the guard holds an
        // ECS read lock and nested reads deadlock behind a queued writer
        let (on_ground, in_water) = (physics.on_ground(), physics.is_in_water());
        drop(physics);
        if on_ground {
            return Err(Error::InvalidState("unable to fly from ground".to_string()));
        }
        if in_water {
            return Err(Error::InvalidState(
                "unable to elytra fly while in water".to_string(),
            ));
        }
        if me
            .get_component::<ActiveEffects>()
            .map(|effects| effects.0.contains_key(&MobEffect::Levitation))
            .unwrap_or(false)
        {
            return Err(Error::InvalidState(
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
}

// ---------------------------------------------------------------------------
// Free helpers (pure — unit-tested)
// ---------------------------------------------------------------------------

/// Decode the `textures` profile property into [`SkinData`]
/// (upstream `extractSkinInformation`).
fn skin_data(profile: &mineflayer_engine::auth::game_profile::GameProfile) -> Option<SkinData> {
    let textures = profile.properties.map.get("textures")?;
    let json = String::from_utf8(base64_decode(&textures.value)?).ok()?;
    // key order inside `textures` (and `SKIN`) is not fixed — read the
    // documented paths, not the byte layout
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let textures = value.get("textures")?;
    let skin = textures.get("SKIN")?;
    Some(SkinData {
        url: skin.get("url")?.as_str()?.to_string(),
        model: skin
            .get("metadata")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
            .map(str::to_string),
        cape_url: textures
            .get("CAPE")
            .and_then(|c| c.get("url"))
            .and_then(|u| u.as_str())
            .map(str::to_string),
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
    use super::*;

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
        let mut properties =
            mineflayer_engine::auth::game_profile::GameProfileProperties::default();
        properties.map.insert(
            "textures".to_string(),
            mineflayer_engine::auth::game_profile::ProfilePropertyValue {
                value: b64.to_string(),
                signature: None,
            },
        );
        let profile = mineflayer_engine::auth::game_profile::GameProfile {
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
        let bare =
            mineflayer_engine::auth::game_profile::GameProfile::new(Default::default(), "x".into());
        assert_eq!(skin_data(&bare), None);
    }

    /// The `SKIN` object's key order is not fixed: Mojang can put
    /// `metadata` before `url`. A regex that stops at the first `}` misses
    /// the URL.
    #[test]
    fn skin_data_parses_metadata_before_url() {
        let b64 = "eyJ0aW1lc3RhbXAiOjAsInByb2ZpbGVJZCI6ImFiYyIsInByb2ZpbGVOYW1lIjoiZmF5ZSIsInRleHR1cmVzIjp7IlNLSU4iOnsibWV0YWRhdGEiOnsibW9kZWwiOiJzbGltIn0sInVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvczIifSwiQ0FQRSI6eyJ1cmwiOiJodHRwOi8vdGV4dHVyZXMubWluZWNyYWZ0Lm5ldC90ZXh0dXJlL2MyIn19fQ==";
        let json = r#"{"timestamp":0,"profileId":"abc","profileName":"faye","textures":{"SKIN":{"metadata":{"model":"slim"},"url":"http://textures.minecraft.net/texture/s2"},"CAPE":{"url":"http://textures.minecraft.net/texture/c2"}}}"#;
        assert_eq!(
            String::from_utf8(base64_decode(b64).unwrap()).unwrap(),
            json
        );
        let mut properties =
            mineflayer_engine::auth::game_profile::GameProfileProperties::default();
        properties.map.insert(
            "textures".to_string(),
            mineflayer_engine::auth::game_profile::ProfilePropertyValue {
                value: b64.to_string(),
                signature: None,
            },
        );
        let profile = mineflayer_engine::auth::game_profile::GameProfile {
            uuid: Default::default(),
            name: "faye".to_string(),
            properties: std::sync::Arc::new(properties),
        };
        assert_eq!(
            skin_data(&profile),
            Some(SkinData {
                url: "http://textures.minecraft.net/texture/s2".to_string(),
                model: Some("slim".to_string()),
                cape_url: Some("http://textures.minecraft.net/texture/c2".to_string()),
            })
        );
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
    fn painting_from_entity_reads_metadata() {
        use mineflayer_engine::registry::{
            DataRegistry as _, data::PaintingVariant as VariantKind,
        };

        let client = crate::test_util::live_entity_client(());
        let e = client
            .ecs
            .write()
            .spawn((
                MinecraftEntityId(7),
                Position::new(Vec3 {
                    x: 1.,
                    y: 2.,
                    z: 3.,
                }),
                mineflayer_engine::entity::metadata::Painting,
                PaintingDirection(Direction::South),
                PaintingVariant(VariantKind::new_raw(2)),
            ))
            .id();
        let painting = Painting::from_entity(&client.entity_ref_for(e)).unwrap();
        assert_eq!(painting.id, MinecraftEntityId(7));
        assert_eq!(
            painting.position,
            Vec3 {
                x: 1.,
                y: 2.,
                z: 3.
            }
        );
        assert_eq!(painting.direction, Direction::South);
        assert_eq!(painting.variant, VariantKind::new_raw(2));

        // a non-painting entity yields None
        let other = client.ecs.write().spawn(MinecraftEntityId(8)).id();
        assert!(Painting::from_entity(&client.entity_ref_for(other)).is_none());
    }

    #[test]
    fn players_attaches_profile_keys() {
        use mineflayer_engine::{
            auth::game_profile::GameProfile, core::game_type::GameMode, local_player::TabList,
        };

        let uuid = uuid::Uuid::from_u128(0x1234);
        let mut tab = TabList::default();
        tab.insert(
            uuid,
            PlayerInfo {
                profile: GameProfile::new(uuid, "faye".into()),
                uuid,
                gamemode: GameMode::Survival,
                latency: 12,
                display_name: None,
            },
        );
        let client = crate::test_util::live_entity_client(tab);
        // `players()` resolves each uuid against `EntityUuidIndex`, a
        // plugin-installed resource this bare ECS doesn't have.
        client
            .ecs
            .write()
            .init_resource::<mineflayer_engine::entity::indexing::EntityUuidIndex>();
        let bot = crate::test_util::bot(client);
        let mut tracker = EntityTracker::default();
        tracker.chat_sessions.insert(
            uuid,
            ProfileKeys {
                public_key: vec![9],
                signature: vec![8],
            },
        );
        *bot.shared.entities.write() = tracker;

        let players = bot.players();
        assert_eq!(
            players["faye"].profile_keys,
            Some(ProfileKeys {
                public_key: vec![9],
                signature: vec![8],
            })
        );
        // players without a recorded session get None
        assert!(players["faye"].entity.is_none());
    }
}
