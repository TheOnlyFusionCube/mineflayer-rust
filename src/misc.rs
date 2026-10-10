//! Misc domains — settings, resource packs, particles & sounds, bed/sleep,
//! fishing, and creative mode — plus the facade-kept state that backs the
//! scoreboard module.
//!
//! azalea stores none of this data (scores, teams, boss bars, pending
//! resource packs, …), so [`MiscState`] is kept as an ECS component on the
//! bot entity: the bridge writes it from decoded packets, `Bot` methods
//! read it.

use std::{collections::HashMap, time::Duration};

// `azalea::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use azalea::ecs as bevy_ecs;
use azalea::{
    ClientInformation, Vec3,
    core::entity_id::MinecraftEntityId,
    ecs::prelude::Component,
    entity::{Pose, Position, inventory::Inventory, metadata::AirSupply},
    inventory::ItemStack,
    protocol::packets::game::{
        ClientboundGamePacket, c_game_event, c_resource_pack_push::ClientboundResourcePackPush,
        s_player_abilities, s_player_command, s_resource_pack, s_set_creative_mode_slot,
    },
    registry::{
        Registry,
        builtin::{BlockKind, EntityKind, ParticleKind, SoundEvent},
    },
};
use tokio::sync::{broadcast, oneshot};

use crate::{Bot, BotEvent, Error, Result, scoreboard};

/// Facade-kept bot state, stored as an ECS component on the client entity.
///
/// Everything upstream keeps in JS-side objects lives here: scoreboards,
/// display slots, teams, boss bars, the resource-pack push awaiting a reply,
/// settings that aren't in [`ClientInformation`], and the diff baselines for
/// pose/air events.
#[derive(Component)]
pub(crate) struct MiscState {
    /// `bot.scoreboards` — objective name -> scoreboard.
    pub scoreboards: HashMap<String, scoreboard::ScoreBoard>,
    /// `ScoreBoard.positions` — display slot -> objective name.
    pub display_slots: HashMap<scoreboard::ScoreboardSlot, String>,
    /// `bot.teams` — team name -> team.
    pub teams: HashMap<String, scoreboard::Team>,
    /// `bot.teamMap` keys — member name -> team name (values are looked up
    /// in `teams`, so the two views can't drift).
    pub team_map: HashMap<String, String>,
    /// Bar uuid string -> bar.
    pub boss_bars: HashMap<String, scoreboard::BossBar>,
    /// The most recent game-phase resource-pack push that hasn't been
    /// answered yet (kept whole so accept/deny reuse the wire uuid — `uuid`
    /// isn't in our dep set, so we never name the type).
    pub latest_resource_pack: Option<ClientboundResourcePackPush>,
    /// `bot.settings.difficulty` — stored client-side like upstream; it is
    /// not a `ClientInformation` field.
    pub difficulty: u8,
    /// Baseline for the `Sleep`/`Wake` diff — `None` until the first tick
    /// read (an uninitialized baseline never emits).
    was_sleeping: Option<bool>,
    /// Baseline for the `Breath` diff — `None` until the first read.
    last_air: Option<i32>,
    /// Cancellation for an in-flight `fish()` — a second call cancels the
    /// first, like upstream.
    fishing: Option<oneshot::Sender<()>>,
    /// Creative slots with an unconfirmed `set_inventory_slot` in flight.
    creative_slots_in_flight: std::collections::HashSet<u16>,
}

impl Default for MiscState {
    fn default() -> Self {
        Self {
            scoreboards: HashMap::new(),
            display_slots: HashMap::new(),
            teams: HashMap::new(),
            team_map: HashMap::new(),
            boss_bars: HashMap::new(),
            latest_resource_pack: None,
            // upstream's default `bot.settings.difficulty`
            difficulty: 2,
            was_sleeping: None,
            last_air: None,
            fishing: None,
            creative_slots_in_flight: Default::default(),
        }
    }
}

impl MiscState {
    /// Objectives and teams don't survive a login (upstream clears them on
    /// the `login` packet; the objects emit no events). Boss bars are
    /// intentionally kept — upstream never clears them either.
    fn reset_on_login(&mut self) {
        self.scoreboards.clear();
        self.display_slots.clear();
        self.teams.clear();
        self.team_map.clear();
    }
}

/// The bridge calls this once per emitted [`BotEvent`]. `Packet` events are
/// decoded into misc-domain state + events; `Tick` drives the pose/air
/// diffs for `Sleep`/`Wake`/`Breath`.
pub(crate) fn after_event(
    client: &azalea::Client,
    bot_event: &BotEvent,
    events: &broadcast::Sender<BotEvent>,
) {
    let extra = match bot_event {
        BotEvent::Packet(packet) => handle_packet(client, packet),
        BotEvent::Tick => tick_diffs(client),
        _ => return,
    };
    for event in extra {
        let _ = events.send(event);
    }
}

/// `Sleep`/`Wake` (from `Pose`) and `Breath` (from `AirSupply`), by diffing
/// components once per game tick. Upstream fires these off the
/// entity-metadata packets; a tick-aligned diff produces the same event
/// sequence at ≤50ms latency.
fn tick_diffs(client: &azalea::Client) -> Vec<BotEvent> {
    let mut events = Vec::new();
    let mut ecs = client.ecs.write();
    let Ok(mut entity) = ecs.get_entity_mut(client.entity) else {
        return events;
    };
    let sleeping = entity.get::<Pose>().map(|pose| *pose == Pose::Sleeping);
    let air = entity.get::<AirSupply>().map(|a| a.0);
    let Some(mut state) = entity.get_mut::<MiscState>() else {
        return events;
    };
    if let Some(sleeping) = sleeping {
        match state.was_sleeping {
            Some(was) if was != sleeping => {
                state.was_sleeping = Some(sleeping);
                events.push(if sleeping {
                    BotEvent::Sleep
                } else {
                    BotEvent::Wake
                });
            }
            None => state.was_sleeping = Some(sleeping),
            _ => {}
        }
    }
    if let Some(air) = air {
        match state.last_air {
            Some(was) if was != air => {
                state.last_air = Some(air);
                events.push(BotEvent::Breath);
            }
            None => state.last_air = Some(air),
            _ => {}
        }
    }
    events
}

/// Decode a clientbound packet into misc-domain state changes + events.
fn handle_packet(client: &azalea::Client, packet: &ClientboundGamePacket) -> Vec<BotEvent> {
    // Entity sounds carry a minecraft entity id instead of a position —
    // resolve it here so the pure handler stays free of ECS access.
    let entity_pos = match packet {
        ClientboundGamePacket::SoundEntity(p) => entity_position(client, p.id),
        _ => None,
    };

    let mut ecs = client.ecs.write();
    let Ok(mut entity) = ecs.get_entity_mut(client.entity) else {
        return vec![];
    };
    if entity.get_mut::<MiscState>().is_none() {
        entity.insert(MiscState::default());
    }
    let Some(mut state) = entity.get_mut::<MiscState>() else {
        return vec![];
    };
    handle_packet_state(&mut state, packet, entity_pos)
}

/// The pure packet → (state, events) reducer. `entity_pos` is the resolved
/// position for `SoundEntity` packets (`None` is fine elsewhere).
fn handle_packet_state(
    state: &mut MiscState,
    packet: &ClientboundGamePacket,
    entity_pos: Option<Vec3>,
) -> Vec<BotEvent> {
    if let Some(events) = scoreboard::handle(state, packet) {
        return events;
    }
    match packet {
        // a fresh login wipes objectives and teams (upstream does the same)
        ClientboundGamePacket::Login(_) => {
            state.reset_on_login();
            vec![]
        }
        ClientboundGamePacket::SetTitleText(p) => vec![BotEvent::Title {
            text: p.text.to_string(),
            kind: TitleKind::Title,
        }],
        ClientboundGamePacket::SetSubtitleText(p) => vec![BotEvent::Title {
            text: p.text.to_string(),
            kind: TitleKind::Subtitle,
        }],
        ClientboundGamePacket::SetTitlesAnimation(p) => vec![BotEvent::TitleTimes {
            fade_in: p.fade_in,
            stay: p.stay,
            fade_out: p.fade_out,
        }],
        ClientboundGamePacket::ClearTitles(_) => vec![BotEvent::TitleClear],
        ClientboundGamePacket::SetHealth(_) => vec![BotEvent::Health],
        ClientboundGamePacket::SetExperience(_) => vec![BotEvent::Experience],
        ClientboundGamePacket::GameEvent(p)
            if p.event == c_game_event::EventType::NoRespawnBlockAvailable =>
        {
            // bed/respawn-anchor spawn was invalidated (upstream's
            // 'spawnReset' lives in the bed plugin)
            vec![BotEvent::SpawnReset]
        }
        ClientboundGamePacket::LevelParticles(p) => {
            vec![BotEvent::ParticleSpawned(Particle::from_packet(p))]
        }
        ClientboundGamePacket::Sound(p) => vec![sound_event(
            &p.sound,
            Vec3::new(
                f64::from(p.x) / 8.0,
                f64::from(p.y) / 8.0,
                f64::from(p.z) / 8.0,
            ),
            p.volume,
            p.pitch,
        )],
        ClientboundGamePacket::SoundEntity(p) => vec![sound_event(
            &p.sound,
            entity_pos.unwrap_or_default(),
            p.volume,
            p.pitch,
        )],
        ClientboundGamePacket::BlockEvent(p) => {
            if p.block != BlockKind::NoteBlock {
                return vec![];
            }
            vec![BotEvent::NoteHeard {
                block: p.pos,
                instrument: Instrument::from(p.action_id),
                pitch: p.action_parameter,
            }]
        }
        ClientboundGamePacket::ResourcePackPush(p) => {
            state.latest_resource_pack = Some(p.clone());
            vec![BotEvent::ResourcePack {
                uuid: p.id.to_string(),
                url: p.url.clone(),
                hash: p.hash.clone(),
                required: p.required,
            }]
        }
        _ => vec![],
    }
}

/// `bot.on('title', (text, kind))` — which title packet produced the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleKind {
    /// `set_title_text`.
    Title,
    /// `set_subtitle_text` — upstream emits `title` for subtitles too.
    Subtitle,
}

/// `bot.on('noteHeard', (block, instrument, pitch))` instruments, in the
/// protocol's wire order (matches upstream's `instruments.json` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instrument {
    Harp,
    Basedrum,
    Snare,
    Hat,
    Bass,
    Flute,
    Bell,
    Guitar,
    Chime,
    Xylophone,
    IronXylophone,
    CowBell,
    Didgeridoo,
    Bit,
    Banjo,
    Pling,
}

impl From<u8> for Instrument {
    /// Wire index -> instrument; out-of-range falls back to `Harp`.
    fn from(id: u8) -> Self {
        const ALL: [Instrument; 16] = [
            Instrument::Harp,
            Instrument::Basedrum,
            Instrument::Snare,
            Instrument::Hat,
            Instrument::Bass,
            Instrument::Flute,
            Instrument::Bell,
            Instrument::Guitar,
            Instrument::Chime,
            Instrument::Xylophone,
            Instrument::IronXylophone,
            Instrument::CowBell,
            Instrument::Didgeridoo,
            Instrument::Bit,
            Instrument::Banjo,
            Instrument::Pling,
        ];
        ALL.get(usize::from(id))
            .copied()
            .unwrap_or(Instrument::Harp)
    }
}

impl Instrument {
    /// Upstream's instrument name (`mcdata.instruments` key).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Harp => "harp",
            Self::Basedrum => "basedrum",
            Self::Snare => "snare",
            Self::Hat => "hat",
            Self::Bass => "bass",
            Self::Flute => "flute",
            Self::Bell => "bell",
            Self::Guitar => "guitar",
            Self::Chime => "chime",
            Self::Xylophone => "xylophone",
            Self::IronXylophone => "iron_xylophone",
            Self::CowBell => "cow_bell",
            Self::Didgeridoo => "didgeridoo",
            Self::Bit => "bit",
            Self::Banjo => "banjo",
            Self::Pling => "pling",
        }
    }
}

/// The `particle` event payload — the upstream `Particle` class fields plus
/// the typed particle data azalea decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct Particle {
    /// `particle.id` — the particle kind's protocol id.
    pub id: u32,
    /// `particle.name` — e.g. `minecraft:bubble`.
    pub name: String,
    /// `particle.position`.
    pub position: Vec3,
    /// `particle.offset` — the per-axis spawn spread.
    pub offset: Vec3,
    /// `particle.longDistanceRender` (the wire's `always_show`).
    pub long_distance_render: bool,
    /// `particle.count`.
    pub count: u32,
    /// `particle.movementSpeed` (the wire's `max_speed`).
    pub movement_speed: f32,
    /// The typed particle kind + data azalea decoded.
    pub data: azalea::entity::particle::Particle,
}

impl Particle {
    /// `Particle.fromNetwork(packet)`.
    pub fn from_packet(
        packet: &azalea::protocol::packets::game::c_level_particles::ClientboundLevelParticles,
    ) -> Self {
        let id = particle_kind_id(&packet.particle).unwrap_or(0);
        let name = ParticleKind::from_u32(id)
            .map(|k| k.to_str().to_string())
            .unwrap_or_else(|| format!("unknown:{id}"));
        Self {
            id,
            name,
            position: packet.pos,
            offset: Vec3::new(
                f64::from(packet.x_dist),
                f64::from(packet.y_dist),
                f64::from(packet.z_dist),
            ),
            long_distance_render: packet.always_show,
            count: packet.count,
            movement_speed: packet.max_speed,
            data: packet.particle.clone(),
        }
    }
}

/// The wire `Particle` enum has no `kind()` accessor — the kind varint is
/// the first field of its encoding, so serialize and read it back.
fn particle_kind_id(particle: &azalea::entity::particle::Particle) -> Option<u32> {
    use azalea::buf::{AzBuf, AzBufVar};
    let mut buf = Vec::new();
    particle.azalea_write(&mut buf).ok()?;
    u32::azalea_read_var(&mut std::io::Cursor::new(&buf[..])).ok()
}

/// Upstream emits `soundEffectHeard` whenever a sound name is known. On the
/// modern protocol azalea's registry `Holder` always resolves a name (an
/// unresolvable id fails the packet read entirely), so
/// `hardcodedSoundEffectHeard` is unreachable — the `BotEvent` variant exists
/// for parity but is never emitted.
fn sound_event(
    sound: &azalea::registry::Holder<SoundEvent, azalea::core::sound::CustomSound>,
    pos: Vec3,
    volume: f32,
    pitch: f32,
) -> BotEvent {
    use azalea::registry::Holder;
    let name = match sound {
        Holder::Reference(event) => event.to_str().to_string(),
        Holder::Direct(custom) => custom.sound_id.to_string(),
    };
    BotEvent::SoundHeard {
        name,
        pos,
        volume,
        pitch,
    }
}

fn entity_position(client: &azalea::Client, id: MinecraftEntityId) -> Option<Vec3> {
    let entity = client.entity_id_by_minecraft_id(id)?;
    client
        .try_query_entity::<&Position, _>(entity, |p| **p)
        .ok()
}

pub use azalea::protocol::common::client_information::{
    ChatVisibility, ModelCustomization, ParticleStatus,
};

/// `bot.settings` — mirrors `ClientInformation` plus the client-side-only
/// `difficulty` upstream also stores.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// `bot.settings.locale` (sent as `language`; default `en_us`).
    pub locale: String,
    /// `bot.settings.viewDistance` in chunks. Upstream's `far`/`normal`/
    /// `short`/`tiny` map to 12/10/8/6.
    pub view_distance: u8,
    /// `bot.settings.chat` — `Full`/`System`/`Hidden` (upstream's
    /// `enabled`/`commandsOnly`/`disabled`).
    pub chat_visibility: ChatVisibility,
    /// `bot.settings.colorsEnabled`.
    pub chat_colors: bool,
    /// `bot.settings.skinParts` — the seven visibility flags.
    pub model_customization: ModelCustomization,
    /// `bot.settings.mainHand`.
    pub main_hand: azalea::entity::HumanoidArm,
    /// `bot.settings.enableTextFiltering`.
    pub text_filtering_enabled: bool,
    /// `bot.settings.enableServerListing`.
    pub allows_listing: bool,
    /// `bot.settings.particleStatus`.
    pub particle_status: ParticleStatus,
    /// `bot.settings.difficulty` — kept client-side only, like upstream.
    pub difficulty: u8,
}

impl Default for Settings {
    /// azalea's `ClientInformation` defaults plus upstream's `difficulty: 2`.
    ///
    /// Divergence: azalea's own defaults differ from upstream's in two
    /// fields (`view_distance` 8 vs `normal`=10; `allows_listing` false vs
    /// upstream's unconditional true).
    fn default() -> Self {
        ClientInformation::default().into()
    }
}

impl From<ClientInformation> for Settings {
    fn from(info: ClientInformation) -> Self {
        Self {
            locale: info.language,
            view_distance: info.view_distance,
            chat_visibility: info.chat_visibility,
            chat_colors: info.chat_colors,
            model_customization: info.model_customization,
            main_hand: info.main_hand,
            text_filtering_enabled: info.text_filtering_enabled,
            allows_listing: info.allows_listing,
            particle_status: info.particle_status,
            difficulty: 2,
        }
    }
}

impl From<&Settings> for ClientInformation {
    fn from(s: &Settings) -> Self {
        Self {
            language: s.locale.clone(),
            view_distance: s.view_distance,
            chat_visibility: s.chat_visibility,
            chat_colors: s.chat_colors,
            model_customization: s.model_customization,
            main_hand: s.main_hand,
            text_filtering_enabled: s.text_filtering_enabled,
            allows_listing: s.allows_listing,
            particle_status: s.particle_status,
        }
    }
}

/// Shared `MiscState` accessor — lazily inserts the component.
fn misc_mut<R>(client: &azalea::Client, f: impl FnOnce(&mut MiscState) -> R) -> Result<R> {
    let mut ecs = client.ecs.write();
    let Ok(mut entity) = ecs.get_entity_mut(client.entity) else {
        return Err(Error::NotConnected);
    };
    if entity.get_mut::<MiscState>().is_none() {
        entity.insert(MiscState::default());
    }
    let mut state = entity.get_mut::<MiscState>().unwrap();
    Ok(f(&mut state))
}

impl Bot {
    /// `bot.settings` — reads back the `ClientInformation` component azalea
    /// stores (so a builder `view_distance` and later `set_settings` calls
    /// are both reflected), plus `difficulty` from facade state.
    pub fn settings(&self) -> Settings {
        let info = self
            .client
            .get_component::<ClientInformation>()
            .map(|i| (*i).clone())
            .unwrap_or_default();
        let difficulty = self
            .client
            .get_component::<MiscState>()
            .map(|m| m.difficulty)
            .unwrap_or(2);
        Settings {
            difficulty,
            ..info.into()
        }
    }

    /// `bot.setSettings(options)` — sends `ClientInformation` and stores the
    /// settings. Merge like upstream with struct update syntax:
    /// `bot.set_settings(Settings { view_distance: 4, ..bot.settings() })`.
    pub fn set_settings(&self, settings: Settings) -> Result<()> {
        // upstream asserts on an invalid view distance
        if settings.view_distance == 0 {
            return Err(Error::Operation(
                "invalid view distance setting: 0".to_string(),
            ));
        }
        let difficulty = settings.difficulty;
        self.client
            .set_client_information(ClientInformation::from(&settings));
        misc_mut(&self.client, |m| m.difficulty = difficulty)?;
        Ok(())
    }

    /// `bot.acceptResourcePack()` — answers the latest game-phase resource
    /// pack push with `Accepted` then `SuccessfullyLoaded` (upstream's two
    /// packet sequence). Config-phase pushes are auto-accepted by azalea's
    /// `AcceptResourcePacksPlugin` (upstream does the same — the server
    /// holds the configuration phase open until they're answered).
    pub fn accept_resource_pack(&self) -> Result<()> {
        let Some(push) = self
            .client
            .get_component::<MiscState>()
            .and_then(|m| m.latest_resource_pack.clone())
        else {
            return Err(Error::Operation("no resource pack to answer".into()));
        };
        for action in [
            s_resource_pack::Action::Accepted,
            s_resource_pack::Action::SuccessfullyLoaded,
        ] {
            self.client
                .write_packet(s_resource_pack::ServerboundResourcePack {
                    id: push.id,
                    action,
                });
        }
        Ok(())
    }

    /// `bot.denyResourcePack()` — answers the latest push with `Declined`.
    ///
    /// Divergence: azalea's `AcceptResourcePacksPlugin` answers every
    /// game-phase push with `Accepted` + `SuccessfullyLoaded` as it
    /// arrives, so a denial races a reply that already went out — the
    /// server will usually ignore it. See api-parity.md.
    pub fn deny_resource_pack(&self) -> Result<()> {
        let Some(push) = self
            .client
            .get_component::<MiscState>()
            .and_then(|m| m.latest_resource_pack.clone())
        else {
            return Err(Error::Operation("no resource pack to answer".into()));
        };
        self.client
            .write_packet(s_resource_pack::ServerboundResourcePack {
                id: push.id,
                action: s_resource_pack::Action::Declined,
            });
        Ok(())
    }

    /// `bot.isABed(block)` — `true` when the block state is any of the 16
    /// colored beds (or the legacy `bed`).
    pub fn is_a_bed(&self, state: azalea::block::BlockState) -> bool {
        let name = state.as_block_kind().to_str();
        name == "minecraft:bed" || name.ends_with("_bed")
    }

    /// `bot.isSleeping` — from the `Pose` entity metadata, like upstream's
    /// flag that its `entitySleep`/`entityWake` events set.
    pub fn is_sleeping(&self) -> bool {
        self.client
            .get_component::<Pose>()
            .map(|p| *p == Pose::Sleeping)
            .unwrap_or(false)
    }

    /// `bot.sleep(bedBlock)` — right-click the bed and wait for the sleeping
    /// pose, like upstream (3s). Upstream's client-side night/monster/
    /// occupancy checks aren't reproduced — the server enforces them, and a
    /// refusal surfaces as `bot is not sleeping`.
    pub async fn sleep(&self, bed: azalea::BlockPos) -> Result<()> {
        if self.is_sleeping() {
            return Err(Error::Operation("already sleeping".into()));
        }
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        {
            let world = self.client.world();
            let world = world.read();
            let Some(state) = world.get_block_state(bed) else {
                return Err(Error::Operation(
                    "there's no bed block at the given position".into(),
                ));
            };
            if !self.is_a_bed(state) {
                return Err(Error::Operation("wrong block : not a bed block".into()));
            }
        }
        // upstream registers the sleep listener before activating the block
        self.client.block_interact(bed);
        for _ in 0..60 {
            if self.is_sleeping() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(Error::Operation("bot is not sleeping".into()))
    }

    /// `bot.wake()` — the stop-sleeping entity action.
    pub fn wake(&self) -> Result<()> {
        if !self.is_sleeping() {
            return Err(Error::Operation("already awake".into()));
        }
        let Some(id) = self
            .client
            .try_query_self::<Option<&MinecraftEntityId>, _>(|e| e.copied())
            .unwrap_or_default()
        else {
            return Err(Error::NotConnected);
        };
        self.client
            .write_packet(s_player_command::ServerboundPlayerCommand {
                id,
                action: s_player_command::Action::StopSleeping,
                data: 0,
            });
        Ok(())
    }

    /// `bot.fish()` — cast the rod, wait for the bite, reel in. Upstream's
    /// detection: the bobber spawns (add_entity), then 6 `fishing`/`bubble`
    /// splash particles land within 1.23 blocks of it (compared on the
    /// horizontal plane); the bobber despawning or a second `fish()` call
    /// cancels the wait.
    pub async fn fish(&self) -> Result<()> {
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        misc_mut(&self.client, |m| {
            if let Some(prev) = m.fishing.take() {
                let _ = prev.send(());
            }
            m.fishing = Some(cancel_tx);
        })?;

        // subscribe before casting so the bobber spawn can't be missed
        let mut rx = self.events();
        let mut bobber: Option<MinecraftEntityId> = None;
        self.client.start_use_item();

        let result = loop {
            let event = tokio::select! {
                event = rx.recv() => match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break Err(Error::NotConnected),
                },
                _ = &mut cancel_rx => break Err(Error::Operation(
                    "Fishing cancelled due to calling bot.fish() again".into(),
                )),
            };
            let BotEvent::Packet(packet) = event else {
                continue;
            };
            match &*packet {
                ClientboundGamePacket::AddEntity(p)
                    if p.entity_type == EntityKind::FishingBobber && bobber.is_none() =>
                {
                    bobber = Some(p.id);
                }
                ClientboundGamePacket::RemoveEntities(p) => {
                    if let Some(id) = bobber
                        && p.entity_ids.contains(&id)
                    {
                        break Err(Error::Operation("Fishing cancelled".into()));
                    }
                }
                ClientboundGamePacket::LevelParticles(p) => {
                    let Some(id) = bobber else { continue };
                    let splash = matches!(
                        p.particle,
                        azalea::entity::particle::Particle::Fishing
                            | azalea::entity::particle::Particle::Bubble
                    );
                    if !(splash && p.count == 6) {
                        continue;
                    }
                    let Some(pos) = entity_position(&self.client, id) else {
                        continue;
                    };
                    // upstream compares on the horizontal plane only
                    let dx = p.pos.x - pos.x;
                    let dz = p.pos.z - pos.z;
                    if dx.hypot(dz) <= 1.23 {
                        self.client.start_use_item();
                        break Ok(());
                    }
                }
                _ => {}
            }
        };

        let _ = misc_mut(&self.client, |m| m.fishing = None);
        result
    }

    /// `bot.creative` — the creative-mode namespace (`set_inventory_slot`,
    /// `fly_to`, `start_flying`, …). The methods only take effect in
    /// creative mode, same as upstream.
    pub fn creative(&self) -> Creative {
        Creative {
            client: self.client.clone(),
            events: self.events(),
        }
    }
}

/// `bot.creative`.
pub struct Creative {
    client: azalea::Client,
    events: broadcast::Receiver<BotEvent>,
}

impl Creative {
    /// `bot.creative.setInventorySlot(slot, item)` — the
    /// `s_set_creative_mode_slot` packet, then a wait for the server's
    /// `container_set_slot` echo like upstream (a mismatch is a rejection,
    /// and upstream rejects after 5s with no echo).
    ///
    /// Upstream asserts `0 <= slot <= 44`; slots are unsigned here.
    pub async fn set_inventory_slot(&self, slot: u16, item: ItemStack) -> Result<()> {
        if slot > 44 {
            return Err(Error::Operation("slot must be <= 44".into()));
        }
        // no-op when the slot already holds an identical stack, like
        // upstream's Item.equal check
        let same = self
            .client
            .get_component::<Inventory>()
            .map(|inv| inv.inventory_menu.slots().get(usize::from(slot)) == Some(&item))
            .unwrap_or(false);
        if same {
            return Ok(());
        }
        let in_flight = misc_mut(&self.client, |m| !m.creative_slots_in_flight.insert(slot))?;
        if in_flight {
            return Err(Error::Operation(format!(
                "Setting slot {slot} cancelled due to calling bot.creative.set_inventory_slot({slot}, ...) again"
            )));
        }
        let result = self.set_inventory_slot_inner(slot, item).await;
        let _ = misc_mut(&self.client, |m| {
            m.creative_slots_in_flight.remove(&slot);
        });
        result
    }

    async fn set_inventory_slot_inner(&self, slot: u16, item: ItemStack) -> Result<()> {
        let mut rx = self.events.resubscribe();
        self.client
            .write_packet(s_set_creative_mode_slot::ServerboundSetCreativeModeSlot {
                slot_num: slot,
                item_stack: item.clone(),
            });
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(BotEvent::Packet(packet)) => {
                        if let ClientboundGamePacket::ContainerSetSlot(p) = &*packet
                            && p.container_id == 0
                            && p.slot == slot
                        {
                            return if p.item_stack == item {
                                Ok(())
                            } else {
                                Err(Error::Operation("Server rejected".into()))
                            };
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::NotConnected),
                }
            }
        };
        match tokio::time::timeout(Duration::from_secs(5), wait).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout),
        }
    }

    /// `bot.creative.clearSlot(slot)`.
    pub async fn clear_slot(&self, slot: u16) -> Result<()> {
        self.set_inventory_slot(slot, ItemStack::Empty).await
    }

    /// `bot.creative.clearInventory()` — clear every occupied slot of the
    /// player inventory.
    pub async fn clear_inventory(&self) -> Result<()> {
        let Some(inv) = self.client.get_component::<Inventory>() else {
            return Err(Error::NotConnected);
        };
        let occupied: Vec<u16> = inv
            .inventory_menu
            .slots()
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_empty())
            .map(|(slot, _)| slot as u16)
            .collect();
        drop(inv);
        for slot in occupied {
            self.clear_slot(slot).await?;
        }
        Ok(())
    }

    /// `bot.creative.startFlying()` — set the flying ability locally and
    /// tell the server (`s_player_abilities`), matching what the vanilla
    /// client does on double-jump. (Upstream zeroes `bot.physics.gravity`;
    /// same effect.)
    pub fn start_flying(&self) {
        self.set_flying(true);
    }

    /// `bot.creative.stopFlying()`.
    pub fn stop_flying(&self) {
        self.set_flying(false);
    }

    fn set_flying(&self, flying: bool) {
        {
            let mut ecs = self.client.ecs.write();
            if let Ok(mut entity) = ecs.get_entity_mut(self.client.entity)
                && let Some(mut abilities) = entity.get_mut::<azalea::entity::PlayerAbilities>()
            {
                abilities.flying = flying;
            }
        }
        self.client
            .write_packet(s_player_abilities::ServerboundPlayerAbilities { is_flying: flying });
    }

    /// `bot.creative.flyTo(destination)` — a straight line like upstream:
    /// keep flying and step the position ~0.5 blocks per 50ms until the
    /// destination is reached.
    pub async fn fly_to(&self, destination: Vec3) {
        self.start_flying();
        loop {
            let pos = self
                .client
                .get_component::<Position>()
                .map(|p| **p)
                .unwrap_or_default();
            let delta = destination - pos;
            let magnitude = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
            if magnitude <= 0.5 {
                self.set_position(destination);
                return;
            }
            self.set_position(pos + delta / magnitude * 0.5);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn set_position(&self, pos: Vec3) {
        let mut ecs = self.client.ecs.write();
        if let Ok(mut entity) = ecs.get_entity_mut(self.client.entity)
            && let Some(mut position) = entity.get_mut::<Position>()
        {
            **position = pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use azalea::{
        core::game_type::{GameMode, OptionalGameType},
        protocol::packets::{
            common::CommonPlayerSpawnInfo,
            game::{
                c_clear_titles::ClientboundClearTitles,
                c_game_event::{ClientboundGameEvent, EventType},
                c_level_particles::ClientboundLevelParticles,
                c_login::ClientboundLogin,
                c_resource_pack_push::ClientboundResourcePackPush,
                c_set_subtitle_text::ClientboundSetSubtitleText,
                c_set_title_text::ClientboundSetTitleText,
                c_set_titles_animation::ClientboundSetTitlesAnimation,
                c_sound::{ClientboundSound, SoundSource},
            },
        },
        registry::{DataRegistry, Holder, data::DimensionKind},
    };

    use super::*;

    fn run(packet: ClientboundGamePacket) -> (MiscState, Vec<BotEvent>) {
        let mut s = MiscState::default();
        let events = handle_packet_state(&mut s, &packet, None);
        (s, events)
    }

    #[test]
    fn title_packets() {
        let (_, e) = run(ClientboundGamePacket::SetTitleText(
            ClientboundSetTitleText {
                text: "Hello".into(),
            },
        ));
        assert!(
            matches!(&e[..], [BotEvent::Title { kind: TitleKind::Title, text }] if text == "Hello")
        );

        let (_, e) = run(ClientboundGamePacket::SetSubtitleText(
            ClientboundSetSubtitleText {
                text: "World".into(),
            },
        ));
        assert!(
            matches!(&e[..], [BotEvent::Title { kind: TitleKind::Subtitle, text }] if text == "World")
        );

        let (_, e) = run(ClientboundGamePacket::SetTitlesAnimation(
            ClientboundSetTitlesAnimation {
                fade_in: 10,
                stay: 70,
                fade_out: 20,
            },
        ));
        assert!(matches!(
            &e[..],
            [BotEvent::TitleTimes {
                fade_in: 10,
                stay: 70,
                fade_out: 20
            }]
        ));

        let (_, e) = run(ClientboundGamePacket::ClearTitles(ClientboundClearTitles {
            reset_times: true,
        }));
        assert!(matches!(&e[..], [BotEvent::TitleClear]));
    }

    #[test]
    fn particle_event_fields() {
        let (_, e) = run(ClientboundGamePacket::LevelParticles(
            ClientboundLevelParticles {
                override_limiter: false,
                always_show: true,
                pos: Vec3::new(1.0, 2.0, 3.0),
                x_dist: 0.1,
                y_dist: 0.2,
                z_dist: 0.3,
                max_speed: 0.5,
                count: 6,
                particle: azalea::entity::particle::Particle::Fishing,
            },
        ));
        let [BotEvent::ParticleSpawned(p)] = &e[..] else {
            panic!("expected one ParticleSpawned, got {e:?}")
        };
        assert_eq!(p.name, "minecraft:fishing");
        assert!(p.long_distance_render);
        assert_eq!(p.count, 6);
        assert_eq!(p.position, Vec3::new(1.0, 2.0, 3.0));
        // offsets go through f32, so compare with tolerance
        assert!(
            (p.offset.x - 0.1).abs() < 1e-6
                && (p.offset.y - 0.2).abs() < 1e-6
                && (p.offset.z - 0.3).abs() < 1e-6
        );
        assert_eq!(p.data, azalea::entity::particle::Particle::Fishing);
        // the id round-trips through the wire encoding
        assert_eq!(p.id, ParticleKind::to_u32(&ParticleKind::Fishing));
    }

    #[test]
    fn sound_event_named_and_custom() {
        // a registry reference yields the builtin name
        let (_, e) = run(ClientboundGamePacket::Sound(ClientboundSound {
            sound: Holder::Reference(SoundEvent::EntityFishingBobberSplash),
            source: SoundSource::Neutral,
            x: 80,
            y: 520,
            z: -160,
            volume: 1.0,
            pitch: 1.5,
            seed: 0,
        }));
        assert!(
            matches!(&e[..], [BotEvent::SoundHeard { name, pos, volume: 1.0, pitch: 1.5 }] if name == "minecraft:entity.fishing_bobber.splash" && *pos == Vec3::new(10.0, 65.0, -20.0))
        );

        // a direct custom sound carries its own identifier
        let (_, e) = run(ClientboundGamePacket::Sound(ClientboundSound {
            sound: Holder::Direct(azalea::core::sound::CustomSound {
                sound_id: azalea::Identifier::new("custom:thing"),
                range: None,
            }),
            source: SoundSource::Master,
            x: 0,
            y: 0,
            z: 0,
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }));
        assert!(matches!(&e[..], [BotEvent::SoundHeard { name, .. }] if name == "custom:thing"));
    }

    #[test]
    fn note_heard_instruments() {
        let packet = |instrument: u8| {
            ClientboundGamePacket::BlockEvent(
                azalea::protocol::packets::game::c_block_event::ClientboundBlockEvent {
                    pos: azalea::BlockPos::new(1, 2, 3),
                    action_id: instrument,
                    action_parameter: 12,
                    block: BlockKind::NoteBlock,
                },
            )
        };
        let (_, e) = run(packet(7));
        assert!(matches!(
            &e[..],
            [BotEvent::NoteHeard {
                instrument: Instrument::Guitar,
                pitch: 12,
                ..
            }]
        ));
        // out of range falls back to harp
        let (_, e) = run(packet(99));
        assert!(matches!(
            &e[..],
            [BotEvent::NoteHeard {
                instrument: Instrument::Harp,
                ..
            }]
        ));
        // non-note-block block events are not ours
        let (_, e) = run(ClientboundGamePacket::BlockEvent(
            azalea::protocol::packets::game::c_block_event::ClientboundBlockEvent {
                pos: azalea::BlockPos::new(0, 0, 0),
                action_id: 0,
                action_parameter: 0,
                block: BlockKind::Piston,
            },
        ));
        assert!(e.is_empty());
    }

    #[test]
    fn game_event_spawn_reset() {
        let (_, e) = run(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
            event: EventType::NoRespawnBlockAvailable,
            param: 0.0,
        }));
        assert!(matches!(&e[..], [BotEvent::SpawnReset]));

        // other game events aren't ours (time/weather is another unit's)
        let (_, e) = run(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
            event: EventType::StartRaining,
            param: 0.0,
        }));
        assert!(e.is_empty());
    }

    #[test]
    fn resource_pack_push_tracked() {
        let mut s = MiscState::default();
        let push = ClientboundResourcePackPush {
            id: azalea::buf::AzBuf::azalea_read(&mut Cursor::new(&[0u8; 16][..])).unwrap(),
            url: "https://packs.example/x.zip".into(),
            hash: "deadbeef".into(),
            required: true,
            prompt: None,
        };
        let events =
            handle_packet_state(&mut s, &ClientboundGamePacket::ResourcePackPush(push), None);
        assert!(
            matches!(&events[..], [BotEvent::ResourcePack { uuid, url, hash, required }] if url == "https://packs.example/x.zip" && hash == "deadbeef" && *required && uuid == "00000000-0000-0000-0000-000000000000")
        );
        assert_eq!(s.latest_resource_pack.unwrap().hash, "deadbeef");
    }

    #[test]
    fn login_resets_scoreboards_and_teams() {
        let mut s = MiscState::default();
        s.scoreboards.insert(
            "o".into(),
            scoreboard::ScoreBoard {
                name: "o".into(),
                title: "t".into(),
                items_map: Default::default(),
            },
        );
        s.display_slots
            .insert(scoreboard::ScoreboardSlot::Sidebar, "o".into());
        s.teams.insert(
            "t".into(),
            scoreboard::Team {
                name: "t".into(),
                display_name: "d".into(),
                friendly_fire: false,
                see_friendly_invisibles: false,
                nametag_visibility: scoreboard::NameTagVisibility::Always,
                collision_rule: scoreboard::CollisionRule::Always,
                color: azalea_chat::style::ChatFormatting::Red,
                prefix: "".into(),
                suffix: "".into(),
                members: Default::default(),
            },
        );
        s.team_map.insert("m".into(), "t".into());
        s.boss_bars.insert(
            "b".into(),
            scoreboard::BossBar {
                entity_uuid: "b".into(),
                title: "t".into(),
                health: 1.0,
                dividers: 0,
                color: azalea::protocol::packets::game::c_boss_event::BossBarColor::Purple,
                darken_sky: false,
                dragon_bar: false,
                create_fog: false,
            },
        );

        let login = ClientboundLogin {
            player_id: MinecraftEntityId(0),
            hardcore: false,
            levels: vec![],
            max_players: 20,
            chunk_radius: 2,
            simulation_distance: 2,
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            common: CommonPlayerSpawnInfo {
                dimension_type: DimensionKind::new_raw(0),
                dimension: azalea::Identifier::new("minecraft:overworld"),
                seed: 0,
                game_type: GameMode::Survival,
                previous_game_type: OptionalGameType(None),
                is_debug: false,
                is_flat: false,
                last_death_location: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            enforces_secure_chat: false,
        };
        let events = handle_packet_state(&mut s, &ClientboundGamePacket::Login(login), None);

        // no events, but scoreboards + teams + the member map are gone —
        // boss bars survive, like upstream
        assert!(events.is_empty());
        assert!(s.scoreboards.is_empty() && s.display_slots.is_empty());
        assert!(s.teams.is_empty() && s.team_map.is_empty());
        assert_eq!(s.boss_bars.len(), 1);
    }

    #[test]
    fn settings_map_to_client_information() {
        let settings = Settings {
            view_distance: 5,
            chat_visibility: ChatVisibility::System,
            main_hand: azalea::entity::HumanoidArm::Left,
            ..Default::default()
        };
        let info = ClientInformation::from(&settings);
        assert_eq!(info.view_distance, 5);
        assert_eq!(info.chat_visibility, ChatVisibility::System);
        assert_eq!(info.main_hand, azalea::entity::HumanoidArm::Left);
        assert_eq!(info.language, "en_us");
        // the skin-parts bitfield keeps all seven flags on by default
        let skin = info.model_customization;
        assert!(
            skin.cape
                && skin.jacket
                && skin.left_sleeve
                && skin.right_sleeve
                && skin.left_pants
                && skin.right_pants
                && skin.hat
        );
        // and a subset can be turned off per flag
        let info = ClientInformation::from(&Settings {
            model_customization: ModelCustomization {
                cape: false,
                jacket: false,
                left_sleeve: true,
                right_sleeve: false,
                left_pants: true,
                right_pants: false,
                hat: false,
            },
            ..Default::default()
        });
        let skin = info.model_customization;
        assert!(
            !skin.cape
                && !skin.jacket
                && skin.left_sleeve
                && !skin.right_sleeve
                && skin.left_pants
                && !skin.right_pants
                && !skin.hat
        );
        // the skin-parts subset round-trips back
        let back = Settings::from(info);
        let skin = back.model_customization;
        assert!(skin.left_sleeve && skin.left_pants && !skin.cape && !skin.hat);
        // and `difficulty` keeps its upstream default
        assert_eq!(back.difficulty, 2);
    }
}
