//! Misc domains — settings, resource packs, particles & sounds, titles,
//! bed/sleep, fishing, and creative mode.
//!
//! The engine stores none of the state here (the pending resource pack,
//! the difficulty setting, the in-flight fish and creative requests), so
//! `MiscState` is an ECS component on the bot entity: the bridge writes it
//! from decoded packets, and `Bot` methods read it.

use std::time::Duration;

// `mineflayer_engine::ecs` is the `bevy_ecs` crate re-exported; the derive needs the
// crate under its real name
use mineflayer_engine::ecs as bevy_ecs;
use mineflayer_engine::{
    ClientInformation, Event, Vec3,
    core::entity_id::MinecraftEntityId,
    ecs::prelude::Component,
    entity::{EntityKindComponent, Pose, Position, metadata::AirSupply},
    events::ResourcePackOffer,
    protocol::packets::{
        config,
        game::{ClientboundGamePacket, c_game_event, s_player_command, s_resource_pack},
    },
    registry::{
        Registry,
        builtin::{EntityKind, ParticleKind, SoundEvent},
    },
};
use tokio::sync::{broadcast, oneshot};

mod creative;
pub use creative::Creative;

use crate::bot::with_component_mut;
use crate::{Bot, BotEvent, Error, Result};

/// Facade-kept bot state, stored as an ECS component on the client entity.
///
/// Everything upstream keeps in JS-side objects lives here: the
/// resource-pack push awaiting a reply, settings that aren't in
/// [`ClientInformation`], and the diff baselines for pose/air events.
/// Scoreboard domain state lives in `scoreboard::ScoreboardState`.
#[derive(Component)]
pub(crate) struct MiscState {
    /// The most recent resource-pack offer, from either phase — kept so
    /// accept/deny can echo the exact uuid the server sent.
    pub latest_resource_pack: Option<ResourcePackOffer>,
    /// `bot.settings.difficulty` — stored client-side like upstream; it is
    /// not a `ClientInformation` field.
    pub difficulty: u8,
    /// Cancellation for an in-flight `fish()` — a second call cancels the
    /// first, like upstream.
    fishing: Option<oneshot::Sender<()>>,
    /// Creative slots with an unconfirmed `set_inventory_slot` in flight.
    creative_slots_in_flight: std::collections::HashSet<u16>,
}

impl Default for MiscState {
    fn default() -> Self {
        Self {
            latest_resource_pack: None,
            // upstream's default `bot.settings.difficulty`
            difficulty: 2,
            fishing: None,
            creative_slots_in_flight: Default::default(),
        }
    }
}

/// The bridge observer. Packets are decoded into misc-domain state and
/// events; `Tick` drives the pose/air diffs for `Sleep`/`Wake`/`Breath`.
pub(crate) fn observe(
    client: &mineflayer_engine::Client,
    baselines: &mut TickBaselines,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        Event::Packet(packet) => handle_packet(client, packet),
        Event::Tick => tick_diffs(client, baselines),
        Event::ResourcePack(offer) => {
            with_component_mut::<MiscState, _>(client, |state| resource_pack_offer(state, offer))
                .unwrap_or_default()
        }
        _ => Vec::new(),
    }
}

/// The last pose and air supply the tick diff saw, kept by the bridge.
/// `None` until the first tick read (an uninitialized baseline never
/// emits).
#[derive(Default)]
pub(crate) struct TickBaselines {
    was_sleeping: Option<bool>,
    last_air: Option<i32>,
}

/// `Sleep`/`Wake` (from `Pose`) and `Breath` (from `AirSupply`), by diffing
/// components once per game tick. Upstream fires these off the
/// entity-metadata packets; a tick-aligned diff produces the same event
/// sequence at ≤50ms latency.
fn tick_diffs(client: &mineflayer_engine::Client, state: &mut TickBaselines) -> Vec<BotEvent> {
    let mut events = Vec::new();
    let (sleeping, air) = {
        let ecs = client.ecs.read();
        let Ok(entity) = ecs.get_entity(client.entity) else {
            return events;
        };
        (
            entity.get::<Pose>().map(|pose| *pose == Pose::Sleeping),
            entity.get::<AirSupply>().map(|a| a.0),
        )
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
fn handle_packet(
    client: &mineflayer_engine::Client,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        // entity sounds carry an entity id instead of a position
        ClientboundGamePacket::SoundEntity(p) => vec![sound_event(
            &p.sound,
            entity_position(client, p.id).unwrap_or_default(),
            p.volume,
            p.pitch,
        )],
        _ => packet_events(packet),
    }
}

/// Keep the offer so that accept/deny can echo the exact uuid the server
/// sent. The engine reports the offers of both phases, so the
/// configuration-phase packs of server.properties and proxies are seen
/// too.
fn resource_pack_offer(state: &mut MiscState, offer: &ResourcePackOffer) -> Vec<BotEvent> {
    state.latest_resource_pack = Some(offer.clone());
    vec![BotEvent::ResourcePack {
        uuid: offer.id,
        url: offer.url.clone(),
        hash: offer.hash.clone(),
        required: offer.required,
    }]
}

/// The packets whose events need no state and no ECS access.
fn packet_events(packet: &ClientboundGamePacket) -> Vec<BotEvent> {
    match packet {
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
/// the typed particle data the engine decoded.
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
    /// The typed particle kind + data the engine decoded.
    pub data: mineflayer_engine::entity::particle::Particle,
}

impl Particle {
    /// `Particle.fromNetwork(packet)`.
    pub fn from_packet(
        packet: &mineflayer_engine::protocol::packets::game::c_level_particles::ClientboundLevelParticles,
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
fn particle_kind_id(particle: &mineflayer_engine::entity::particle::Particle) -> Option<u32> {
    use mineflayer_engine::buf::{McBuf, McBufVar};
    let mut buf = Vec::new();
    particle.mc_write(&mut buf).ok()?;
    u32::mc_read_var(&mut std::io::Cursor::new(&buf[..])).ok()
}

/// Upstream emits `soundEffectHeard` whenever a sound name is known. On the
/// modern protocol the engine's registry `Holder` always resolves a name (an
/// unresolvable id fails the packet read entirely), so
/// `hardcodedSoundEffectHeard` is unreachable — the `BotEvent` variant exists
/// for parity but is never emitted.
fn sound_event(
    sound: &mineflayer_engine::registry::Holder<
        SoundEvent,
        mineflayer_engine::core::sound::CustomSound,
    >,
    pos: Vec3,
    volume: f32,
    pitch: f32,
) -> BotEvent {
    use mineflayer_engine::registry::Holder;
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

fn entity_position(client: &mineflayer_engine::Client, id: MinecraftEntityId) -> Option<Vec3> {
    let entity = crate::entity::entity_id_by_minecraft_id(client, id)?;
    client
        .try_query_entity::<&Position, _>(entity, |p| **p)
        .ok()
}

pub use mineflayer_engine::entity::HumanoidArm;
pub use mineflayer_engine::protocol::common::client_information::{
    ChatVisibility, ModelCustomization, ParticleStatus,
};

/// Upstream's `viewDistance` value — a chunk count or one of the named
/// presets (`'far'`/`'normal'`/`'short'`/`'tiny'` → 12/10/8/6 chunks,
/// settings.js's `viewDistanceToBits`). `u8` converts to
/// [`ViewDistance::Chunks`]; the alias names parse via [`str::parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewDistance {
    /// `'far'` — 12 chunks.
    Far,
    /// `'normal'` — 10 chunks.
    Normal,
    /// `'short'` — 8 chunks.
    Short,
    /// `'tiny'` — 6 chunks.
    Tiny,
    /// A raw chunk count (upstream accepts any positive number).
    Chunks(u8),
}

impl ViewDistance {
    /// The chunk count the client-information packet carries.
    pub fn chunks(self) -> u8 {
        match self {
            Self::Far => 12,
            Self::Normal => 10,
            Self::Short => 8,
            Self::Tiny => 6,
            Self::Chunks(n) => n,
        }
    }
}

impl From<u8> for ViewDistance {
    fn from(chunks: u8) -> Self {
        Self::Chunks(chunks)
    }
}

impl From<ViewDistance> for u8 {
    fn from(v: ViewDistance) -> u8 {
        v.chunks()
    }
}

/// Compares against the resolved chunk count, so
/// `settings.view_distance == 8` still reads naturally.
impl PartialEq<u8> for ViewDistance {
    fn eq(&self, other: &u8) -> bool {
        self.chunks() == *other
    }
}

/// Parses upstream's string form (`"far"`, `"normal"`, `"short"`,
/// `"tiny"`) plus bare chunk counts.
impl std::str::FromStr for ViewDistance {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "far" => Self::Far,
            "normal" => Self::Normal,
            "short" => Self::Short,
            "tiny" => Self::Tiny,
            _ => Self::Chunks(s.parse::<u8>().map_err(|_| {
                Error::InvalidArgument(format!("invalid view distance setting: {s}"))
            })?),
        })
    }
}

/// `bot.settings` — mirrors `ClientInformation` plus the client-side-only
/// `difficulty` upstream also stores.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// `bot.settings.locale` (sent as `language`; default `en_us`).
    pub locale: String,
    /// `bot.settings.viewDistance` — [`ViewDistance::Far`]/[`ViewDistance::Normal`]/
    /// [`ViewDistance::Short`]/[`ViewDistance::Tiny`] for upstream's string
    /// presets (12/10/8/6 chunks), or [`ViewDistance::Chunks`] for a raw
    /// count.
    pub view_distance: ViewDistance,
    /// `bot.settings.chat` — `Full`/`System`/`Hidden` (upstream's
    /// `enabled`/`commandsOnly`/`disabled`).
    pub chat_visibility: ChatVisibility,
    /// `bot.settings.colorsEnabled`.
    pub chat_colors: bool,
    /// `bot.settings.skinParts` — the seven visibility flags.
    pub model_customization: ModelCustomization,
    /// `bot.settings.mainHand`.
    pub main_hand: mineflayer_engine::entity::HumanoidArm,
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
    /// The engine's `ClientInformation` defaults plus upstream's `difficulty: 2`.
    ///
    /// Divergence: the engine's own defaults differ from upstream's in two
    /// fields (`view_distance` 8 vs `far`=12; `allows_listing` false vs
    /// upstream's unconditional true).
    fn default() -> Self {
        ClientInformation::default().into()
    }
}

impl From<ClientInformation> for Settings {
    fn from(info: ClientInformation) -> Self {
        Self {
            locale: info.language,
            view_distance: info.view_distance.into(),
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
            view_distance: s.view_distance.chunks(),
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

impl Bot {
    /// `bot.settings` — reads back the `ClientInformation` component the engine
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
    /// `bot.set_settings(Settings { view_distance: 4.into(), ..bot.settings() })`.
    pub fn set_settings(&self, settings: Settings) -> Result<()> {
        // upstream asserts on an invalid view distance
        if settings.view_distance == 0 {
            return Err(Error::InvalidArgument(
                "invalid view distance setting: 0".to_string(),
            ));
        }
        let difficulty = settings.difficulty;
        self.client
            .set_client_information(ClientInformation::from(&settings));
        with_component_mut::<MiscState, _>(&self.client, |m| m.difficulty = difficulty)
            .ok_or(Error::NotConnected)?;
        Ok(())
    }

    /// `bot.acceptResourcePack()` — answers the latest offer with
    /// `Accepted` then `SuccessfullyLoaded`, in the phase the client is in
    /// (configuration or game). With
    /// [`BotBuilder::accept_resource_packs(false)`](crate::BotBuilder::accept_resource_packs)
    /// this and [`Bot::deny_resource_pack`] are the only answers; else the
    /// engine accepts each offer when it arrives.
    pub fn accept_resource_pack(&self) -> Result<()> {
        self.answer_resource_pack(&[
            (
                s_resource_pack::Action::Accepted,
                config::s_resource_pack::Action::Accepted,
            ),
            (
                s_resource_pack::Action::SuccessfullyLoaded,
                config::s_resource_pack::Action::SuccessfullyLoaded,
            ),
        ])
    }

    /// `bot.denyResourcePack()` — answers the latest offer with `Declined`.
    /// It takes effect with
    /// [`BotBuilder::accept_resource_packs(false)`](crate::BotBuilder::accept_resource_packs);
    /// else the engine has already accepted the offer.
    pub fn deny_resource_pack(&self) -> Result<()> {
        self.answer_resource_pack(&[(
            s_resource_pack::Action::Declined,
            config::s_resource_pack::Action::Declined,
        )])
    }

    /// Send `answers` for the latest offer: the game form, or the
    /// configuration form while the client is in that phase.
    fn answer_resource_pack(
        &self,
        answers: &[(s_resource_pack::Action, config::s_resource_pack::Action)],
    ) -> Result<()> {
        let Some(id) = self
            .client
            .get_component::<MiscState>()
            .and_then(|m| m.latest_resource_pack.as_ref().map(|offer| offer.id))
        else {
            return Err(Error::InvalidState("no resource pack to answer".into()));
        };
        let in_config = self
            .client
            .get_component::<mineflayer_engine::InConfigState>()
            .is_some();
        for (game, config) in answers {
            if in_config {
                self.client.ecs.write().commands().trigger(
                    mineflayer_engine::packet::config::SendConfigPacketEvent::new(
                        self.client.entity,
                        config::ServerboundResourcePack {
                            id,
                            action: *config,
                        },
                    ),
                );
            } else {
                self.client
                    .write_packet(s_resource_pack::ServerboundResourcePack { id, action: *game });
            }
        }
        Ok(())
    }

    /// `bot.isABed(block)` — `true` when the block state is any of the 16
    /// colored beds (or the legacy `bed`).
    pub fn is_a_bed(&self, state: mineflayer_engine::block::BlockState) -> bool {
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
    pub async fn sleep(&self, bed: mineflayer_engine::BlockPos) -> Result<()> {
        if self.is_sleeping() {
            return Err(Error::InvalidState("already sleeping".into()));
        }
        if !self.logged_in() {
            return Err(Error::NotConnected);
        }
        {
            let world = self.client.world();
            let world = world.read();
            let Some(state) = world.get_block_state(bed) else {
                return Err(Error::NotFound(
                    "there's no bed block at the given position".into(),
                ));
            };
            if !self.is_a_bed(state) {
                return Err(Error::InvalidArgument(
                    "wrong block : not a bed block".into(),
                ));
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
        Err(Error::Failed("bot is not sleeping".into()))
    }

    /// `bot.wake()` — the stop-sleeping entity action.
    pub fn wake(&self) -> Result<()> {
        if !self.is_sleeping() {
            return Err(Error::InvalidState("already awake".into()));
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
        with_component_mut::<MiscState, _>(&self.client, |m| {
            if let Some(prev) = m.fishing.take() {
                let _ = prev.send(());
            }
            m.fishing = Some(cancel_tx);
        })
        .ok_or(Error::NotConnected)?;

        // subscribe before casting so the bobber spawn can't be missed
        let mut rx = self.subscribe();
        let mut bobber: Option<MinecraftEntityId> = None;
        self.client.start_use_item();

        // the `events` sender lives on `Bot`, so `rx` can never report
        // `Closed` while `self` is held — select on the bridge's root
        // token (and match `End`) or the loop parks forever on disconnect.
        // `End` is the fast path; the root token covers the lagged-out case where
        // >8192 queued events push `End` past the receiver
        let root = self.root.clone();
        let result = loop {
            let event = tokio::select! {
                event = rx.recv() => match event {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break Err(Error::NotConnected),
                },
                _ = &mut cancel_rx => break Err(Error::Cancelled(
                    "Fishing cancelled due to calling bot.fish() again".into(),
                )),
                _ = root.cancelled() => break Err(Error::NotConnected),
            };
            if matches!(event, BotEvent::End(_)) {
                break Err(Error::NotConnected);
            }
            // derived events, not raw packets, so fishing also works with
            // `packet_events(false)`: `EntitySpawn` (add_entity),
            // `EntityGone` (remove_entities), `ParticleSpawned`
            // (level_particles)
            match event {
                BotEvent::EntitySpawn(entity) if bobber.is_none() => {
                    let is_bobber = entity
                        .get_component::<EntityKindComponent>()
                        .is_some_and(|kind| kind.0 == EntityKind::FishingBobber);
                    if is_bobber {
                        bobber = entity.get_component::<MinecraftEntityId>().map(|id| *id);
                    }
                }
                BotEvent::EntityGone { id, .. } if Some(id) == bobber => {
                    break Err(Error::Cancelled("Fishing cancelled".into()));
                }
                BotEvent::ParticleSpawned(p) => {
                    let Some(id) = bobber else { continue };
                    let splash = matches!(
                        p.data,
                        mineflayer_engine::entity::particle::Particle::Fishing
                            | mineflayer_engine::entity::particle::Particle::Bubble
                    );
                    if !(splash && p.count == 6) {
                        continue;
                    }
                    let Some(pos) = entity_position(&self.client, id) else {
                        continue;
                    };
                    // upstream compares on the horizontal plane only
                    let dx = p.position.x - pos.x;
                    let dz = p.position.z - pos.z;
                    if dx.hypot(dz) <= 1.23 {
                        self.client.start_use_item();
                        break Ok(());
                    }
                }
                _ => {}
            }
        };

        let _ = with_component_mut::<MiscState, _>(&self.client, |m| m.fishing = None);
        result
    }
}

#[cfg(test)]
mod tests;
