//! `Bot` — the connected bot handle, the Rust analog of the object mineflayer's
//! `createBot()` returns.
//!
//! [`Bot::builder`] / [`BotBuilder::connect`] are the `createBot(options)`
//! analog. Events are delivered over a [`tokio::sync::broadcast`] channel
//! (the multi-subscriber, lag-tolerant EventEmitter analog): call
//! [`Bot::events`] to subscribe.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use azalea::{
    ClientInformation, EntityRef, Vec3,
    auto_reconnect::AutoReconnectDelay,
    core::{
        position::{BlockPos, ChunkPos},
        registry_holder::RegistryHolder,
    },
    entity::HumanoidArm,
    player::PlayerInfo,
    protocol::{
        connect::Proxy,
        packets::game::{
            ClientboundGamePacket, c_block_entity_data::ClientboundBlockEntityData,
            c_sound::SoundSource,
        },
    },
    world::WorldName,
};
use std::sync::RwLock;
use tokio::sync::{broadcast, watch};

use crate::{
    Error, Result, bridge,
    chat::{ChatPatternMatch, ChatShared, MessagePosition},
    misc::{ChatVisibility, Instrument, ModelCustomization, Particle, ParticleStatus, TitleKind},
    scoreboard::{BossBar, ScoreBoard, ScoreItem, ScoreboardSlot, Team},
    world::{Block, WorldState},
};

/// A connected bot — the Rust analog of the object returned by mineflayer's
/// `createBot()`.
///
/// `Bot` is a cheap cloneable handle wrapping [`azalea::Client`]. High-level
/// mineflayer methods (`bot.chat`, `bot.dig`, `bot.pathfinder`, …) are added
/// to it as `impl Bot` blocks in the domain modules.
///
/// When the last `Bot` handle is dropped, the client is disconnected and the
/// underlying ECS runner is shut down.
#[derive(Clone)]
pub struct Bot {
    pub(crate) client: azalea::Client,
    events: broadcast::Sender<BotEvent>,
    /// Reason passed to [`Bot::quit`]/[`Bot::end`]; the bridge consumes it when
    /// azalea reports the disconnect so `BotEvent::End` can carry it
    /// (upstream: `client.end(reason)` -> `'end'` event).
    pub(crate) quit_reason: Arc<Mutex<Option<String>>>,
    /// Dropping the last sender signals the bridge task to shut the client
    /// down (see `bridge::spawn`). Held only to keep the watch channel open.
    _alive: watch::Sender<()>,
    /// The bridge's shutdown signal: its sender lives in the bridge task
    /// and drops when the bridge exits (disconnect, or every `Bot`
    /// handle gone), resolving `changed()` for every receiver. The
    /// tick-driven driver tasks (`pathfinder` follow/dynamic watchers,
    /// `pvp::drive`) select on it — their `TickBroadcast` receiver can
    /// never report `Closed` because the sender is a World resource kept
    /// alive by the `Client` handle the driver itself holds.
    pub(crate) shutdown: watch::Receiver<()>,
    /// Scaffold bots ([`Bot::new`]) have no bridge to own the shutdown
    /// sender; keeping it here leaves `shutdown` open — a wrapped client
    /// is never "ended" from our side, matching pre-signal behavior.
    _shutdown_tx: Option<watch::Sender<()>>,
    /// Chat-domain state shared with the bridge (pattern registry, tablist
    /// header/footer, `tab_complete` correlation) — see `chat::ChatShared`.
    pub(crate) chat: Arc<Mutex<ChatShared>>,
    /// Facade-kept world/game state (spawn point, clocks, weather, login
    /// metadata) maintained by the packet bridge in [`crate::world`] —
    /// azalea drops most of these packets upstream.
    pub(crate) world_state: Arc<RwLock<WorldState>>,
    /// Entity-domain state the ECS doesn't keep (equipment, raw attributes,
    /// vehicle links), shared with the bridge task — see `entity::EntityTracker`.
    pub(crate) entities: Arc<std::sync::RwLock<crate::entity::EntityTracker>>,
    /// Inventory/window-domain state (current window, slot snapshots,
    /// eat/use bookkeeping), shared with the bridge task — see
    /// `windows::WindowTracker`.
    pub(crate) windows: Arc<Mutex<crate::windows::WindowTracker>>,
    /// Typed-window server state (container data, merchant offers,
    /// readiness), shared with the bridge task — see
    /// `window_types::WindowDataTracker`.
    pub(crate) window_data: Arc<Mutex<crate::window_types::WindowDataTracker>>,
    /// Recipe-book state (unlocked recipes from `c_recipe_book_add`/
    /// `c_recipe_book_remove`), shared with the bridge task — see
    /// `craft::RecipeBook`.
    pub(crate) recipes: Arc<Mutex<crate::craft::RecipeBook>>,
    /// `options.chatLengthLimit` — the split limit `chat`/`whisper` chunk
    /// outgoing messages to (set once at build; upstream has no setter).
    pub(crate) chat_length_limit: usize,
}

/// Events emitted by a [`Bot`], mirroring mineflayer's `bot.on(...)` events.
///
/// Bridged from azalea's [`Event`](azalea::Event) enum; variant payloads use
/// azalea's types so no data is lost in translation.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum BotEvent {
    /// The client just started, before login. No direct mineflayer analog
    /// (closest: `inject_allowed`); used internally to apply `view_distance`
    /// before the server sees our client information.
    Init,
    /// `login` — the server accepted our login.
    Login,
    /// `spawn` — the bot spawned into a loaded world.
    Spawn,
    /// `respawn` — emitted right before every `Spawn` after the first
    /// (respawn after death, dimension change), matching mineflayer's
    /// respawn-then-spawn ordering.
    Respawn,
    /// `death` — the bot died.
    Death,
    /// `kicked` — the server disconnected us with a reason (in-game kicks
    /// only; see api-parity.md for the login-phase caveat).
    Kicked(String),
    /// `end` — the connection ended, with the reason (`socketClosed`,
    /// the `quit`/`end` argument, or a kick reason).
    End(String),
    /// `error` — a non-fatal error was surfaced (connection failures;
    /// the keepalive watchdog's `client timed out` before it disconnects —
    /// upstream's keepalive.js emits `error` then `end('keepAliveError')`).
    Error(String),
    /// `chat` — a chat packet arrived (raw payload; the derived
    /// `Message`/`MessageStr`/`Whisper`/`ChatPattern` family follows it in
    /// emission order).
    Chat(azalea::chat::ChatPacket),
    /// `physicsTick` — a game tick elapsed while the world was loaded.
    Tick,
    /// Raw clientbound packet escape hatch for APIs azalea doesn't lift
    /// (scoreboards, boss bars, titles, particles, …). Emitted under azalea's
    /// `packet-event` feature, which is on by default.
    Packet(Arc<ClientboundGamePacket>),
    /// `playerJoined` — a player was added to the tab list.
    PlayerJoined(PlayerInfo),
    /// `playerUpdated` — a tab-list entry changed (gamemode, display name,
    /// latency).
    PlayerUpdated(PlayerInfo),
    /// `playerLeft` — a player was removed from the tab list.
    PlayerLeft(PlayerInfo),
    /// `chunkColumnLoad` — a chunk column was received.
    ChunkColumnLoad(ChunkPos),
    /// `move` — the bot's position or look direction changed this tick
    /// (upstream fires on every sent movement packet; the per-tick
    /// component diff is the azalea equivalent). Carries the previous
    /// position, like upstream's `oldPos` argument.
    Move {
        /// The bot's position before this move.
        old_pos: azalea::Vec3,
    },
    /// `forcedMove` — the server placed us (teleport, spawn).
    ForcedMove,
    /// `abilities` — the server sent new player abilities; the payload is
    /// what the `PlayerAbilities` component now holds.
    Abilities(azalea::entity::PlayerAbilities),
    /// `whisper` — an incoming whisper addressed to the bot
    /// (`ChatPacket::is_whisper` heuristic; upstream derives this event
    /// from its deprecated default chat patterns instead).
    Whisper(azalea::chat::ChatPacket),
    /// `message` — any incoming message (chat, system, or game_info
    /// position). The upstream `sender` UUID is on the packet
    /// (`sender_uuid()`); upstream's `verified` flag has no azalea analog.
    Message {
        /// The packet the message arrived in.
        packet: azalea::chat::ChatPacket,
        /// Where the message was displayed.
        position: MessagePosition,
    },
    /// `messagestr` — the plain-text form of `message` (the string that
    /// chat patterns and `await_message` match on). `packet` carries
    /// upstream's `originalMsg` argument.
    MessageStr {
        /// The rendered message text.
        message: String,
        /// Where the message was displayed.
        position: MessagePosition,
        /// The packet the message arrived in.
        packet: azalea::chat::ChatPacket,
    },
    /// `actionBar` — game_info/overlay text, from mc26's dedicated
    /// `c_set_action_bar_text` packet or an overlay system-chat packet.
    ActionBar(azalea::FormattedText),
    /// `chat:{name}` — a registered chat pattern (set) completed;
    /// `name` is the pattern name, `matches` mirrors upstream's payload.
    ChatPattern {
        /// The pattern's registered name (the `chat:{name}` suffix).
        name: String,
        /// Whole matched messages, or per-pattern capture groups when the
        /// pattern was added with `parse: true`.
        matches: ChatPatternMatch,
    },
    // — misc domains (scoreboards, teams, boss bars, titles, particles,
    // sounds, health/experience, bed, resource packs) —
    /// `scoreboardCreated` — a scoreboard objective was added.
    ScoreboardCreated(ScoreBoard),
    /// `scoreboardDeleted` — an objective was removed. Payload is the
    /// removed board (upstream emits the same object before deleting).
    ScoreboardDeleted(ScoreBoard),
    /// `scoreboardTitleChanged` — an objective's display name changed.
    ScoreboardTitleChanged(ScoreBoard),
    /// `scoreUpdated` — a score was set on an objective.
    ScoreUpdated {
        /// The objective the score landed on.
        scoreboard: ScoreBoard,
        /// The score entry that was added or updated.
        item: ScoreItem,
    },
    /// `scoreRemoved` — a score was removed from an objective.
    ScoreRemoved {
        /// The objective the score was removed from.
        scoreboard: ScoreBoard,
        /// The score entry that was removed.
        item: ScoreItem,
    },
    /// `scoreboardPosition` — an objective was bound to a display slot.
    ScoreboardPosition {
        /// Where the objective is shown (sidebar, list, team sidebar, …).
        slot: ScoreboardSlot,
        /// The objective now shown in that slot.
        scoreboard: ScoreBoard,
    },
    /// `teamCreated` — a team was added (already populated with members).
    TeamCreated(Team),
    /// `teamRemoved` — a team was removed. Payload is the removed team
    /// (upstream emits `undefined` — it deletes before emitting).
    TeamRemoved(Team),
    /// `teamUpdated` — team parameters (not members) changed.
    TeamUpdated(Team),
    /// `teamMemberAdded` — payload is the updated team, like upstream.
    TeamMemberAdded(Team),
    /// `teamMemberRemoved` — payload is the updated team, like upstream.
    TeamMemberRemoved(Team),
    /// `bossBarCreated` (undocumented upstream).
    BossBarCreated(BossBar),
    /// `bossBarDeleted` — payload is the removed bar (upstream emits the
    /// same object before deleting).
    BossBarDeleted(BossBar),
    /// `bossBarUpdated` — progress/title/style/flags changed.
    BossBarUpdated(BossBar),
    /// `title` — fires for both `set_title_text` (`kind == Title`) and
    /// `set_subtitle_text` (`kind == Subtitle`), like upstream.
    Title {
        /// The plain text content.
        text: String,
        /// Which packet produced the event (upstream's second arg).
        kind: TitleKind,
    },
    /// `title_times` — title fade-in/stay/fade-out in ticks.
    TitleTimes {
        /// Fade-in ticks.
        fade_in: u32,
        /// Display ticks.
        stay: u32,
        /// Fade-out ticks.
        fade_out: u32,
    },
    /// `title_clear`.
    TitleClear,
    /// `particle` — a `level_particles` packet.
    ParticleSpawned(Particle),
    /// `soundEffectHeard` — a named sound effect or entity sound.
    /// `hardcodedSoundEffectHeard` is unreachable on the modern protocol
    /// (azalea always resolves the registry id to a name); the variant
    /// exists for parity but is never emitted.
    SoundHeard {
        /// The sound identifier, e.g. `minecraft:entity.experience_orb.pickup`.
        name: String,
        /// The source position (entity sounds are resolved to the entity's
        /// current position; `Vec3::ZERO` if it isn't loaded).
        pos: Vec3,
        /// Playback volume.
        volume: f32,
        /// Playback pitch.
        pitch: f32,
    },
    /// `hardcodedSoundEffectHeard` — see [`BotEvent::SoundHeard`].
    HardcodedSound {
        /// The sound's registry id.
        id: u32,
        /// The sound category (master, music, hostile, …).
        category: SoundSource,
        /// The source position.
        pos: Vec3,
        /// Playback volume.
        volume: f32,
        /// Playback pitch.
        pitch: f32,
    },
    /// `noteHeard` — a note block was played.
    NoteHeard {
        /// The note block's position (upstream passes a `Block`).
        block: BlockPos,
        /// The instrument, decoded from the block-event action id.
        instrument: Instrument,
        /// The note pitch (0–24), the block-event action parameter.
        pitch: u8,
    },
    /// `resourcePack` — the server pushed a resource pack. Only *game-phase*
    /// pushes produce this event: azalea's `Event::Packet` wraps
    /// `ClientboundGamePacket`, so config-phase pushes (server.properties /
    /// proxy packs — the dominant modern path) never reach it. azalea's
    /// `AcceptResourcePacksPlugin` also auto-accepts every push (see
    /// [`Bot::deny_resource_pack`]/api-parity.md).
    ResourcePack {
        /// The pack's wire UUID.
        uuid: String,
        /// The download URL.
        url: String,
        /// The expected SHA-1 hash.
        hash: String,
        /// Whether the server requires the pack to join.
        required: bool,
    },
    /// `sleep` — the bot's pose became `Sleeping`.
    Sleep,
    /// `wake` — the bot's pose left `Sleeping`.
    Wake,
    /// `breath` — the air supply changed.
    Breath,
    /// `health` — a `set_health` packet arrived.
    Health,
    /// `experience` — a `set_experience` packet arrived.
    Experience,
    /// `spawnReset` — a bed/respawn-anchor spawn point was invalidated.
    SpawnReset,
    /// `chunkColumnUnload` — a chunk column was unloaded by the server.
    ChunkUnload(ChunkPos),
    /// `blockUpdate` — a block changed in the world. `old` is `None` when
    /// the previous state wasn't observed; `new` is what the server
    /// reported. Azalea applies updates to the world itself on the next
    /// tick, so `block_at` may briefly still return `old`.
    ///
    /// mineflayer's position-filtered `blockUpdate:x,y,z` is spelled by
    /// matching `new.pos` on this variant.
    BlockUpdate { old: Option<Block>, new: Block },
    /// `blockEntityData` — raw block-entity NBT. Azalea doesn't store
    /// block entities, so the packet is forwarded as received.
    BlockEntityData(ClientboundBlockEntityData),
    /// `signOpen` — the server opened a sign editor (usually right after
    /// a sign was placed); answer with `Bot::update_sign`.
    SignOpen {
        block: Option<Block>,
        is_front_text: bool,
    },
    /// `pistonMove` — a piston extended (`is_pulling: false`) or
    /// retracted. `direction` is the raw protocol facing byte
    /// (0=down, 1=up, 2=north, 3=south, 4=west, 5=east).
    PistonMove {
        block: Block,
        is_pulling: bool,
        direction: u8,
    },
    /// `chestLidMove` — the open-count of a chest or other lidded
    /// container changed. `second` is the other half for double chests.
    ChestLidMove {
        block: Block,
        open_count: u8,
        second: Option<Block>,
    },
    /// `blockBreakProgressObserved` — a break-animation stage (0–9).
    BlockBreakProgress {
        block: Option<Block>,
        stage: u8,
        entity: Option<EntityRef>,
    },
    /// `blockBreakProgressEnd` — the break animation was removed.
    BlockBreakEnd {
        block: Option<Block>,
        entity: Option<EntityRef>,
    },
    /// `time` — a `set_time` packet updated the world clocks.
    Time,
    /// `rain` — `is_raining` flipped.
    Rain,
    /// `weatherUpdate` — the rain or thunder level changed.
    WeatherUpdate,
    /// `game` — game state changed (login/respawn metadata, gamemode,
    /// default spawn position).
    GameStateChanged,
    /// `entitySpawn` — an entity appeared in the world (also emitted for the
    /// bot itself right after `Spawn`, like upstream).
    EntitySpawn(azalea::EntityRef),
    /// `entityGone` — an entity was removed from the world. `entity` is the
    /// last-seen handle (`None` if the entity was never resolved); `id`
    /// always carries the protocol id since the handle can no longer report it.
    EntityGone {
        /// Last-seen handle for the removed entity, if known.
        entity: Option<azalea::EntityRef>,
        /// The removed entity's protocol id.
        id: azalea::core::entity_id::MinecraftEntityId,
    },
    /// `entityMoved` — an entity's position, look, or head rotation changed.
    EntityMoved(azalea::EntityRef),
    /// `entityUpdate` — an entity's metadata changed.
    EntityUpdate(azalea::EntityRef),
    /// `entityAttributes` — an entity's attributes were updated.
    EntityAttributes(azalea::EntityRef),
    /// `entitySwingArm` — an entity swung its (main-hand) arm.
    EntitySwingArm(azalea::EntityRef),
    /// `entityHurt` — an entity took damage (hurt animation, entity status,
    /// or `c_damage_event`; `source` names the responsible entity when the
    /// server sent one).
    EntityHurt {
        /// The entity that was hurt.
        entity: azalea::EntityRef,
        /// The entity that caused the damage, if the server identified one.
        source: Option<azalea::EntityRef>,
    },
    /// `entityDead` — an entity died (entity-event status 3).
    EntityDead(azalea::EntityRef),
    /// `entityTaming` — tameable entity is being tamed (status 6).
    EntityTaming(azalea::EntityRef),
    /// `entityTamed` — entity was tamed (status 7).
    EntityTamed(azalea::EntityRef),
    /// `entityShakingOffWater` — entity shook off water (status 8).
    EntityShakingOffWater(azalea::EntityRef),
    /// `entityEatingGrass` — entity ate grass (status 10).
    EntityEatingGrass(azalea::EntityRef),
    /// `entityHandSwap` — entity swapped hands (status 55); tracked equipment
    /// is swapped too, like upstream.
    EntityHandSwap(azalea::EntityRef),
    /// `entityWake` — entity woke up (animation 2).
    EntityWake(azalea::EntityRef),
    /// `entityEat` — upstream maps animation 3 to `entityEat`; see
    /// api-parity.md for the off-hand-swing caveat.
    EntityEat(azalea::EntityRef),
    /// `entityCriticalEffect` — critical hit particles (animation 4).
    EntityCriticalEffect(azalea::EntityRef),
    /// `entityMagicCriticalEffect` — magic critical particles (animation 5).
    EntityMagicCriticalEffect(azalea::EntityRef),
    /// `entityCrouch` — a metadata packet flagged the entity as sneaking
    /// (fires per packet carrying the flag, like upstream).
    EntityCrouch(azalea::EntityRef),
    /// `entityUncrouch` — entity stopped sneaking (transition only).
    EntityUncrouch(azalea::EntityRef),
    /// `entitySleep` — a metadata packet flagged the entity as sleeping
    /// (fires per packet carrying the flag, like upstream).
    EntitySleep(azalea::EntityRef),
    /// `entityElytraFlew` — entity started elytra flight (metadata edge).
    EntityElytraFlew(azalea::EntityRef),
    /// `entityEquip` — entity equipment changed (`c_set_equipment`); read the
    /// slots back via [`crate::entity::EntityExt`] / `Bot::equipment`.
    EntityEquip(azalea::EntityRef),
    /// `entityAttach` — entity was attached to a vehicle/holder
    /// (`c_set_entity_link`, e.g. leashes).
    EntityAttach {
        /// The attached entity.
        entity: azalea::EntityRef,
        /// The vehicle/holder entity, if resolvable.
        vehicle: Option<azalea::EntityRef>,
    },
    /// `entityDetach` — entity was detached (`vehicle` is the previous
    /// holder, like upstream).
    EntityDetach {
        /// The detached entity.
        entity: azalea::EntityRef,
        /// The vehicle/holder it was detached from, if known.
        vehicle: Option<azalea::EntityRef>,
    },
    /// `entityEffect` — a mob effect was applied (`c_update_mob_effect`).
    EntityEffect {
        /// The affected entity.
        entity: azalea::EntityRef,
        /// The effect kind.
        effect: azalea::registry::builtin::MobEffect,
        /// Amplifier/duration/flags, mirroring upstream's
        /// `{id, amplifier, duration}` payload.
        data: azalea::entity::MobEffectData,
    },
    /// `entityEffectEnd` — a mob effect was removed (`c_remove_mob_effect`).
    EntityEffectEnd {
        /// The affected entity.
        entity: azalea::EntityRef,
        /// The effect kind that ended.
        effect: azalea::registry::builtin::MobEffect,
        /// The effect's tracked parameters (upstream emits the stored
        /// effect object); `amplifier`/`duration` are `-1` when the effect
        /// was never seen, mirroring upstream's unknown-effect fallback.
        data: azalea::entity::MobEffectData,
    },
    /// `itemDrop` — an entity's metadata carried an item stack (fires for
    /// item entities when their stack metadata arrives).
    ItemDrop(azalea::EntityRef),
    /// `playerCollect` — an entity collected another (`c_take_item_entity`);
    /// `collected` is usually an item entity.
    PlayerCollect {
        /// The collecting entity (usually a player).
        collector: Option<azalea::EntityRef>,
        /// The collected entity.
        collected: Option<azalea::EntityRef>,
    },
    /// `usedFirework` — a firework rocket attached to us was registered
    /// (also feeds `bot.fireworkRocketDuration`).
    UsedFirework(azalea::core::entity_id::MinecraftEntityId),
    /// `mount` — we started riding an entity (`c_set_passengers`).
    Mounted {
        /// The vehicle entity, if resolvable.
        vehicle: Option<azalea::EntityRef>,
    },
    /// `dismount` — we stopped riding (`vehicle` is the previous vehicle,
    /// like upstream; also emitted when the vehicle entity goes away).
    Dismounted {
        /// The vehicle we were riding.
        vehicle: Option<azalea::EntityRef>,
    },
    /// `diggingCompleted` — a [`Bot::dig`] finished; the payload is the new
    /// (usually air) block at the dug position, like upstream's `newBlock`.
    DiggingCompleted(Block),
    /// `diggingAborted` — a [`Bot::dig`] ended without breaking the block.
    DiggingAborted {
        /// The block that was being dug.
        block: Block,
        /// Why the dig ended.
        reason: crate::interact::DigAbortReason,
    },
    /// `blockPlaced` — the server confirmed a [`Bot::place_block`].
    BlockPlaced {
        /// The block previously at the position, if observed.
        old: Option<Block>,
        /// The block the server placed.
        new: Block,
    },
    /// `entityPlaced` (undocumented upstream) — the entity a
    /// [`Bot::place_entity`] spawned.
    EntityPlaced(azalea::EntityRef),
    /// `goal_updated` — `pathfinder.set_goal` installed a new goal
    /// (upstream emits `(goal, dynamic)`).
    GoalUpdated {
        /// The goal that was installed.
        goal: crate::pathfinder::Goal,
        /// The `set_goal` `dynamic` argument it was installed with.
        dynamic: bool,
    },
    /// `path_update` — a new path was found, or a calculation finished
    /// (azalea also ends a failed calculation this way, matching
    /// upstream's per-result `path_update`; `path` is empty then).
    /// `is_partial` mirrors upstream's `r.partial` flag.
    PathUpdate {
        /// The path node list (empty while no path is executing).
        path: Vec<BlockPos>,
        /// Whether the path only gets partway to the goal.
        is_partial: bool,
    },
    /// `goal_reached` — the current goal's success check holds.
    /// Suppressed for `dynamic` goals, like upstream; `goal` is the
    /// reached goal (upstream emits `(goal)`).
    GoalReached {
        /// The goal whose success check now holds.
        goal: crate::pathfinder::Goal,
    },
    /// `path_stop` — the goal was abandoned without reaching. Upstream
    /// emits no payload; `goal` is the facade goal that was cleared, if
    /// one was set.
    PathStop {
        /// The abandoned goal, if one was set.
        goal: Option<crate::pathfinder::Goal>,
    },
    /// `path_reset` — the path was restarted mid-flight. `reason` is
    /// `goal_updated` or `movements_updated`, the two upstream call sites
    /// the facade can observe. azalea's internal repaths (stuck, block
    /// change, chunk load, …) surface as `path_update` instead — see
    /// api-parity.md.
    PathReset {
        /// Which `set_goal`/`set_movements` call caused the reset.
        reason: &'static str,
    },
    // — inventory/windows domain —
    /// `windowOpen` — the server opened a non-inventory window and its
    /// slot data arrived (upstream's `setWindowItems` gate).
    WindowOpen(crate::windows::Window),
    /// `windowClose` — the window was closed by us or the server. The
    /// payload is the window that closed (upstream emits it too — note
    /// upstream's payload can be `null` on some paths; ours never is).
    WindowClose(crate::windows::Window),
    /// `window.updateSlot` — a slot in a window changed. `window_id` is
    /// the protocol window id (0 = player inventory); `old` is the
    /// previously tracked contents (`None` if the slot was empty or
    /// never seen).
    WindowUpdateSlot {
        /// Protocol window id (0 = the player inventory).
        window_id: i32,
        /// Absolute slot index in the window.
        slot: u16,
        /// Previous contents (`None` = was empty/unknown).
        old: Option<crate::windows::Item>,
        /// New contents (`None` = now empty).
        new: Option<crate::windows::Item>,
    },
    /// `heldItemChanged` — the held item changed, or the quickbar
    /// selection moved (upstream also emits it unconditionally from
    /// `updateHeldItem`, so listeners should diff on kind+count like
    /// upstream's eat listener does).
    HeldItemChanged(Option<crate::windows::Item>),
    // — typed windows (U9): `container_set_data`/`merchant_offers`
    // decode. Window-level events stay bot-level with the window id
    // they belong to (windows aren't emitters, same as upstream's
    // `craft_progress_bar` feed consumers match on). —
    /// A `container_set_data` property changed on a window — the raw
    /// feed the typed windows consume (furnace fuel/progress, enchant
    /// clues, anvil repair cost). Upstream's `craft_progress_bar`.
    WindowDataUpdate {
        /// Protocol window id the property belongs to.
        window_id: i32,
        /// The property index (menu-specific).
        property: u16,
        /// The new value (sign-extended from the wire's u16).
        value: i16,
    },
    /// furnace `'update'` — a data property changed on a furnace-kind
    /// window (furnace/blast_furnace/smoker).
    FurnaceUpdate {
        /// Protocol window id of the furnace window.
        window_id: i32,
    },
    /// enchantment table `'ready'` — all three enchant-option level
    /// costs are known; fires on the false→true edge like upstream.
    EnchantmentTableReady {
        /// Protocol window id of the enchantment table.
        window_id: i32,
    },
    /// villager `'ready'` — the first `merchant_offers` packet for the
    /// window arrived and `trades` is populated.
    VillagerReady {
        /// Protocol window id of the merchant window.
        window_id: i32,
    },
    // — ecosystem plugins (U12): collectBlock / pvp progress events.
    // Emitted by the plugin methods themselves (no upstream packet
    // source — mineflayer-collectblock emits nothing; ours surfaces the
    // progress the single-await API can't otherwise report). —
    /// `collect` targeted a matching block (before walking to it).
    CollectBlockFound(Block),
    /// `collect` broke a target block.
    Collected(Block),
    /// `collect` finished — `collected` is how many blocks were broken
    /// (short of the requested count when the search exhausted early).
    CollectDone {
        /// Blocks broken by the completed `collect` call.
        collected: usize,
    },
    /// mineflayer-pvp `stoppedAttacking` — the attack loop ended
    /// (explicit `stop`, target died/despawned). No payload, like
    /// upstream.
    StoppedAttacking,
}

/// `BotEvent` broadcast ring size, shared by every `Bot`. A busy world
/// emits several thousand events a second (one `Packet` per clientbound
/// packet plus per-entity domain events), so 256 overflowed mid-burst
/// and lagged subscribers silently missed events; 8192 holds a
/// multi-second flood without the multi-MB upfront ring a much larger
/// capacity would allocate per `Bot`.
const EVENT_CHANNEL_CAPACITY: usize = 8192;

/// `createBot(options)` analog. Defaults match upstream:
/// `localhost:25565`, username `Player`, offline auth.
///
/// ```no_run
/// # async fn example() -> mineflayer::Result<()> {
/// let bot = mineflayer::Bot::builder()
///     .host("localhost")
///     .port(25566)
///     .username("ferris")
///     .connect()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct BotBuilder {
    /// `options.host` (default `localhost`).
    host: String,
    /// `options.port` (default `25565`).
    port: u16,
    /// `options.username` (default `Player`). Used when `auth` isn't set.
    username: String,
    /// `options.auth` (default `Offline { username }`).
    auth: Option<Auth>,
    /// `options.viewDistance` — applied via `set_client_information` on
    /// `Event::Init`, so the login packet already carries it.
    view_distance: Option<u8>,
    /// The `options.*` client-information fields (`chat`, `colorsEnabled`,
    /// `skinParts`, `mainHand`, `enableTextFiltering`, `enableServerListing`,
    /// `particleStatus`) — merged over azalea's [`ClientInformation`]
    /// defaults and sent on `Event::Init`; `None` until one of them is set.
    client_information: Option<ClientInformation>,
    /// `options.brand` — sent as the `minecraft:brand` custom payload on
    /// `Event::Login`, overwriting the `"vanilla"` azalea already sent
    /// during the config phase (upstream writes it on the login packet;
    /// the server keeps the last brand it received).
    brand: Option<String>,
    /// `options.difficulty` — stored client-side only, like upstream's
    /// `bot.settings.difficulty` (default `2`); the server is authoritative.
    difficulty: u8,
    /// `options.chatLengthLimit` — `chat`/`whisper` split limit
    /// (default [`crate::chat::CHAT_LENGTH_LIMIT`], 256 — mc26 always takes
    /// upstream's >=1.11 branch).
    chat_length_limit: usize,
    /// `options.defaultChatPatterns` (default `true`) — `false` suppresses
    /// [`BotEvent::Whisper`], our stand-in for upstream's deprecated default
    /// whisper patterns.
    default_chat_patterns: bool,
    /// `options.checkTimeoutInterval` — the keepalive watchdog: the bridge
    /// ends the connection with `End("keepAliveError")` when the server goes
    /// this long without a keepalive (default `30s`, upstream's default;
    /// armed on the first keepalive like upstream).
    check_timeout_interval: Duration,
    /// `options.agent`-analog — azalea's SOCKS5 proxy support
    /// (`Client::join_with_proxy`); also covers the `connect` proxy use case.
    proxy: Option<azalea::protocol::connect::Proxy>,
}

/// How the bot authenticates — covers `auth`, `accessToken`, and prebuilt
/// accounts. (No password auth: Mojang password login is dead upstream too;
/// azalea uses the Microsoft device-code flow or access tokens.)
#[derive(Clone)]
pub enum Auth {
    /// `auth: 'offline'` — no Mojang login; `username` is sent as-is.
    Offline {
        /// The username presented to the server.
        username: String,
    },
    /// `auth: 'microsoft'` — azalea's interactive device-code flow (prints a
    /// link code; caches tokens in `~/.minecraft/azalea-auth-cache.json`).
    Microsoft {
        /// Microsoft account email — the token cache key.
        email: String,
    },
    /// A prebuilt [`azalea::account::Account`] — e.g.
    /// `Account::with_microsoft_access_token(token)` (the `accessToken`
    /// option) or a custom `AccountTrait` impl.
    Account(azalea::account::Account),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Account` (Arc<dyn AccountTrait>) has no Debug; don't leak tokens.
        match self {
            Auth::Offline { username } => f
                .debug_struct("Offline")
                .field("username", username)
                .finish(),
            Auth::Microsoft { email } => f.debug_struct("Microsoft").field("email", email).finish(),
            Auth::Account(_) => f.write_str("Account(..)"),
        }
    }
}

impl Default for BotBuilder {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 25565,
            username: "Player".to_string(),
            auth: None,
            view_distance: None,
            client_information: None,
            brand: None,
            difficulty: 2,
            chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
            default_chat_patterns: true,
            check_timeout_interval: Duration::from_secs(30),
            proxy: None,
        }
    }
}

impl BotBuilder {
    /// `options.host`.
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }
    /// `options.port`.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
    /// `options.username`.
    pub fn username(mut self, username: impl Into<String>) -> Self {
        self.username = username.into();
        self
    }
    /// `options.auth`. An explicit [`Auth::Offline`] wins over `username`.
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }
    /// `options.viewDistance`, in chunks.
    pub fn view_distance(mut self, view_distance: u8) -> Self {
        self.view_distance = Some(view_distance);
        self
    }

    /// `options.chat` — upstream's `'enabled'`/`'commandsOnly'`/`'disabled'`
    /// as [`ChatVisibility::Full`]/[`ChatVisibility::System`]/
    /// [`ChatVisibility::Hidden`]. Sent on `Event::Init`.
    pub fn chat(mut self, chat: ChatVisibility) -> Self {
        self.client_information_mut().chat_visibility = chat;
        self
    }
    /// `options.colorsEnabled` (upstream default `true`).
    pub fn colors_enabled(mut self, enabled: bool) -> Self {
        self.client_information_mut().chat_colors = enabled;
        self
    }
    /// `options.skinParts` — the seven model-customization flags
    /// (upstream default: all shown).
    pub fn skin_parts(mut self, skin_parts: ModelCustomization) -> Self {
        self.client_information_mut().model_customization = skin_parts;
        self
    }
    /// `options.mainHand` — `'left'`/`'right'` as [`HumanoidArm`]
    /// (upstream default `'right'`).
    pub fn main_hand(mut self, main_hand: HumanoidArm) -> Self {
        self.client_information_mut().main_hand = main_hand;
        self
    }
    /// `options.enableTextFiltering` (upstream default `false`).
    pub fn enable_text_filtering(mut self, enabled: bool) -> Self {
        self.client_information_mut().text_filtering_enabled = enabled;
        self
    }
    /// `options.enableServerListing`. Divergence: upstream's initializer is
    /// `options.enableServerListing || true`, so the flag is dead and the
    /// packet always sends `true`; here the flag actually applies, over
    /// azalea's default `false`.
    pub fn enable_server_listing(mut self, enabled: bool) -> Self {
        self.client_information_mut().allows_listing = enabled;
        self
    }
    /// `options.particleStatus`. Divergence: upstream parses the option but
    /// hardcodes `'all'` into `bot.settings` — the value it accepts is never
    /// sent; here it lands on the wire like the option intends.
    pub fn particle_status(mut self, status: ParticleStatus) -> Self {
        self.client_information_mut().particle_status = status;
        self
    }
    /// `options.brand` (upstream default `'vanilla'`) — the client brand the
    /// server records, sent as the `minecraft:brand` custom payload once the
    /// login packet arrives. azalea also sends `"vanilla"` earlier during
    /// the config phase; the server keeps the last brand received, so this
    /// wins.
    pub fn brand(mut self, brand: impl Into<String>) -> Self {
        self.brand = Some(brand.into());
        self
    }
    /// `options.difficulty` (upstream default `2`) — stored client-side
    /// only, readable as `bot.settings().difficulty`; never sent (the
    /// server is authoritative, same as upstream).
    pub fn difficulty(mut self, difficulty: u8) -> Self {
        self.difficulty = difficulty;
        self
    }
    /// `options.chatLengthLimit` — the limit `chat`/`whisper` split outgoing
    /// messages to (upstream default `256` on modern versions).
    pub fn chat_length_limit(mut self, limit: usize) -> Self {
        self.chat_length_limit = limit;
        self
    }
    /// `options.defaultChatPatterns` (upstream default `true`). Pass `false`
    /// to suppress [`BotEvent::Whisper`] — the event our port produces in
    /// place of upstream's deprecated default whisper patterns.
    /// [`BotEvent::Chat`] is unaffected: it is the raw chat-packet stream,
    /// not a pattern product.
    pub fn default_chat_patterns(mut self, enabled: bool) -> Self {
        self.default_chat_patterns = enabled;
        self
    }
    /// `options.checkTimeoutInterval` (upstream default `30*1000`ms) — how
    /// long the server may go without a keepalive before the bridge ends
    /// the connection, emitting `BotEvent::Error` then
    /// [`BotEvent::End`] with `"keepAliveError"` like upstream. The watchdog
    /// arms on the first keepalive received, matching upstream's per-
    /// keepalive `setTimeout` re-arm.
    pub fn check_timeout_interval(mut self, mut interval: Duration) -> Self {
        // upstream's `options.checkTimeoutInterval || 30*1000`: a falsy
        // value means the default, so ZERO selects it rather than an
        // instant timeout
        if interval.is_zero() {
            interval = Duration::from_secs(30);
        }
        self.check_timeout_interval = interval;
        self
    }
    /// `options.agent`-analog — connect through a SOCKS5 proxy
    /// (azalea's `Client::join_with_proxy`; covers the server connection,
    /// not Mojang session auth).
    pub fn proxy(mut self, proxy: Proxy) -> Self {
        self.proxy = Some(proxy);
        self
    }

    fn client_information_mut(&mut self) -> &mut ClientInformation {
        self.client_information
            .get_or_insert_with(ClientInformation::default)
    }

    /// `createBot` — resolve the address, start the client, spawn the event
    /// bridge, and return the bot handle.
    ///
    /// Like upstream, this returns before the bot is in the world: connection
    /// problems surface later as [`BotEvent::Error`] + [`BotEvent::End`] on
    /// [`Bot::events`] rather than from this call (only address-resolution
    /// failures are returned directly).
    ///
    /// The connection and ECS run on a dedicated thread — azalea's ECS runner
    /// uses `tokio::task::spawn_local`, which requires a `LocalSet` context —
    /// so `connect` works from any async context, including the default
    /// `#[tokio::main]`. The thread exits when the client does.
    pub async fn connect(self) -> Result<Bot> {
        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let thread_name = format!("mineflayer-bot-{}", self.username);
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("mineflayer: build bot runtime");
                rt.block_on(async {
                    let local = tokio::task::LocalSet::new();
                    local.spawn_local(async move {
                        let _ = init_tx.send(Self::connect_azalea(self).await);
                    });
                    // Drive the LocalSet until all local tasks finish. azalea's
                    // ECS runner is a local task; it exits when the client does
                    // (last `Bot` dropped → `client.exit()`, or fatal error).
                    local.await;
                });
            })
            .map_err(|_| Error::Connect("could not spawn the bot thread".into()))?;

        init_rx
            .await
            .map_err(|_| Error::Connect("bot thread exited during connect".into()))?
    }

    /// Join the server and wire up the event bridge. Runs inside the bot
    /// thread's `LocalSet` (azalea requires it for `Client::join`).
    async fn connect_azalea(builder: BotBuilder) -> Result<Bot> {
        let account = match builder.auth {
            Some(Auth::Offline { username }) => azalea::account::Account::offline(&username),
            Some(Auth::Microsoft { email }) => azalea::account::Account::microsoft(&email)
                .await
                .map_err(Error::Auth)?,
            Some(Auth::Account(account)) => account,
            None => azalea::account::Account::offline(&builder.username),
        };
        let address = format!("{}:{}", builder.host, builder.port);
        let (client, rx) = match &builder.proxy {
            // `options.agent`-analog: azalea's SOCKS5 proxy join path
            Some(proxy) => {
                azalea::Client::join_with_proxy(account, address.as_str(), proxy.clone()).await?
            }
            None => azalea::Client::join(account, address.as_str()).await?,
        };

        // azalea's default plugins auto-reconnect 5s after any disconnect —
        // including our own `disconnect()`. That would silently break
        // `end`/`quit`/`kicked` semantics, so opt out for this client
        // (Duration::MAX is azalea's documented "disabled" value).
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(AutoReconnectDelay::new(Duration::MAX));

        // the misc domains keep their state in one component so it exists
        // before the first packet arrives (difficulty comes from
        // `options.difficulty`; it's client-side only like upstream)
        let mut misc_state = crate::misc::MiscState::default();
        misc_state.difficulty = builder.difficulty;
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(misc_state);

        // dig bookkeeping (lastDigTime / abort generations) — same rationale
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(crate::interact::InteractShared::default());

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let quit_reason = Arc::new(Mutex::new(None));
        let mut chat_shared = ChatShared::default();
        chat_shared.default_chat_patterns = builder.default_chat_patterns;
        let chat = Arc::new(Mutex::new(chat_shared));
        let (alive, alive_rx) = watch::channel(());
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let world_state = Arc::new(RwLock::new(WorldState::default()));
        let world_alive = alive_rx.clone();
        let entities = Arc::new(std::sync::RwLock::new(
            crate::entity::EntityTracker::default(),
        ));
        let windows = Arc::new(Mutex::new(crate::windows::WindowTracker::new(
            events.clone(),
        )));
        let window_data = Arc::new(Mutex::new(crate::window_types::WindowDataTracker::new()));
        let recipes = Arc::new(Mutex::new(crate::craft::RecipeBook::default()));
        bridge::spawn(
            client.clone(),
            rx,
            events.clone(),
            builder.view_distance,
            alive_rx,
            shutdown_tx,
            bridge::BridgeDeps {
                quit_reason: quit_reason.clone(),
                chat: chat.clone(),
                entities: entities.clone(),
                windows: windows.clone(),
                window_data: window_data.clone(),
                recipes: recipes.clone(),
                client_information: builder.client_information,
                brand: builder.brand,
                check_timeout_interval: builder.check_timeout_interval,
            },
        );
        crate::world::spawn_packet_bridge(
            client.clone(),
            events.clone(),
            world_state.clone(),
            world_alive,
            shutdown_rx.clone(),
        );

        Ok(Bot {
            client,
            events,
            quit_reason,
            _alive: alive,
            shutdown: shutdown_rx,
            _shutdown_tx: None,
            chat,
            world_state,
            entities,
            windows,
            window_data,
            recipes,
            chat_length_limit: builder.chat_length_limit,
        })
    }
}

impl Bot {
    /// `createBot(options)` — the builder entry point.
    pub fn builder() -> BotBuilder {
        BotBuilder::default()
    }

    /// Wrap an already-connected [`azalea::Client`] in a `Bot` handle.
    ///
    /// Scaffolding: the resulting bot has no event bridge (no azalea event
    /// receiver exists for prebuilt clients), so [`Bot::events`] yields a
    /// silent stream. Prefer [`Bot::builder`].
    #[doc(hidden)]
    pub fn new(client: azalea::Client) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let (alive, _) = watch::channel(());
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        Self {
            client,
            events: events.clone(),
            quit_reason: Arc::new(Mutex::new(None)),
            _alive: alive,
            shutdown: shutdown_rx,
            _shutdown_tx: Some(shutdown_tx),
            chat: Arc::new(Mutex::new(ChatShared::default())),
            world_state: Arc::new(RwLock::new(WorldState::default())),
            entities: Arc::new(std::sync::RwLock::new(
                crate::entity::EntityTracker::default(),
            )),
            windows: Arc::new(Mutex::new(crate::windows::WindowTracker::new(events))),
            window_data: Arc::new(Mutex::new(crate::window_types::WindowDataTracker::new())),
            recipes: Arc::new(Mutex::new(crate::craft::RecipeBook::default())),
            chat_length_limit: crate::chat::CHAT_LENGTH_LIMIT,
        }
    }

    /// The underlying [`azalea::Client`] — the `bot._client` analog for when
    /// the facade doesn't cover something yet.
    pub fn client(&self) -> &azalea::Client {
        &self.client
    }

    /// Subscribe to this bot's event stream.
    ///
    /// Each call returns an independent receiver; slow consumers see
    /// [`broadcast::error::RecvError::Lagged`] rather than blocking the bot.
    pub fn events(&self) -> broadcast::Receiver<BotEvent> {
        self.events.subscribe()
    }

    /// `bot.quit(reason?)` — graceful disconnect. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `disconnect.quitting`).
    pub fn quit(&self, reason: Option<&str>) {
        *self.quit_reason.lock().unwrap() =
            Some(reason.unwrap_or("disconnect.quitting").to_string());
        self.client.disconnect();
    }

    /// `bot.end(reason?)` — end the connection. The subsequent
    /// [`BotEvent::End`] carries `reason` (default `socketClosed`).
    pub fn end(&self, reason: Option<&str>) {
        *self.quit_reason.lock().unwrap() = reason.map(str::to_string);
        self.client.disconnect();
    }

    /// `bot.respawn()` — manual respawn; a no-op while alive (upstream
    /// semantics). azalea auto-respawns on death (like mineflayer's default
    /// `respawn: true`), so this mainly exists for parity.
    pub fn respawn(&self) {
        if !self.is_alive() {
            use azalea::protocol::packets::game::s_client_command::{
                Action, ServerboundClientCommand,
            };
            self.client.write_packet(ServerboundClientCommand {
                action: Action::PerformRespawn,
            });
        }
    }

    /// `bot.username` — the (possibly server-adjusted) account name; empty
    /// before the game profile arrives.
    pub fn username(&self) -> String {
        self.client
            .get_component::<azalea::player::GameProfileComponent>()
            .map(|profile| profile.name.clone())
            .unwrap_or_default()
    }

    /// `bot.version` — the Minecraft version azalea targets (single-version
    /// port; see api-parity.md).
    pub fn version(&self) -> &'static str {
        azalea::protocol::packets::VERSION_NAME
    }

    /// `bot.protocolVersion`.
    pub fn protocol_version(&self) -> i32 {
        azalea::protocol::packets::PROTOCOL_VERSION
    }

    /// `bot.majorVersion` — single-version port: same as [`Bot::version`].
    pub fn major_version(&self) -> &'static str {
        azalea::protocol::packets::VERSION_NAME
    }

    /// `bot.registry` — a snapshot of the server's dynamic registries
    /// (dimensions, enchantments, damage kinds, …). Static data
    /// (blocks/items/entities) lives in `azalea::registry` / `azalea::block`.
    pub fn registry(&self) -> RegistryHolder {
        self.client.with_registry_holder(|r| r.clone())
    }

    /// `bot.waitForTicks(n)` — resolves after `n` game ticks. Times out after
    /// `n * 50ms + 5s` like upstream (ticks stop when the world unloads).
    pub async fn wait_for_ticks(&self, ticks: u32) -> Result<()> {
        let limit = Duration::from_millis(u64::from(ticks) * 50 + 5000);
        tokio::time::timeout(limit, self.client.wait_ticks(ticks as usize))
            .await
            .map_err(|_| Error::Timeout)
    }

    /// `bot.waitForChunksToLoad()` — resolves once every chunk in the 5×5
    /// area centered on the bot's position is present in the client-loaded
    /// slice of the world (azalea's `PartialWorld`). 10s timeout, same as
    /// upstream.
    pub async fn wait_for_chunks_to_load(&self) -> Result<()> {
        const TIMEOUT: Duration = Duration::from_secs(10);
        tokio::time::timeout(TIMEOUT, async {
            let mut rx = self.events();
            loop {
                if self.spawn_chunks_loaded() {
                    return Ok(());
                }
                match rx.recv().await {
                    // any event may have changed the world — just re-check
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return if self.spawn_chunks_loaded() {
                            Ok(())
                        } else {
                            Err(Error::NotConnected)
                        };
                    }
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// The `waitForChunksToLoad` condition: the bot entity is in the world
    /// and all 25 chunks of the 5×5 area centered on it are loaded.
    fn spawn_chunks_loaded(&self) -> bool {
        if !self.client.exists() {
            return false;
        }
        let pos = self.position();
        // `>> 4` matches upstream's int32 shift semantics
        let (cx, cz) = ((pos.x as i32) >> 4, (pos.z as i32) >> 4);
        let world = self.client.partial_world();
        let world = world.read();
        for dx in -2..=2 {
            for dz in -2..=2 {
                if world
                    .chunks
                    .limited_get(&ChunkPos::new(cx + dx, cz + dz))
                    .is_none()
                {
                    return false;
                }
            }
        }
        true
    }

    /// Emit a [`BotEvent`] to all subscribers — used by domain impls for
    /// upstream's `bot.emit('error', ...)` semantics inside sync methods.
    pub(crate) fn emit_event(&self, event: BotEvent) {
        let _ = self.events.send(event);
    }

    /// Whether we've received the login packet and are in the game state.
    /// Unlike `azalea::Client::logged_in` this never panics — it returns
    /// `false` once the entity is gone.
    pub(crate) fn logged_in(&self) -> bool {
        self.client
            .try_query_self::<Option<&WorldName>, _>(|w| w.is_some())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_event_broadcast_reaches_all_subscribers() {
        let (tx, mut rx1) = broadcast::channel::<BotEvent>(16);
        let mut rx2 = tx.subscribe();

        tx.send(BotEvent::Login).unwrap();
        tx.send(BotEvent::Kicked("bye".to_string())).unwrap();

        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Login));
        assert!(matches!(rx1.try_recv().unwrap(), BotEvent::Kicked(_)));
        assert!(matches!(rx2.try_recv().unwrap(), BotEvent::Kicked(_)));
    }

    #[test]
    fn builder_defaults() {
        let b = Bot::builder();
        assert_eq!(b.host, "localhost");
        assert_eq!(b.port, 25565);
        assert_eq!(b.username, "Player");
        assert!(b.auth.is_none());
        assert!(b.view_distance.is_none());
        let b = b
            .host("example.org")
            .port(25566)
            .username("x")
            .view_distance(4);
        assert_eq!(b.host, "example.org");
        assert_eq!(b.port, 25566);
        assert_eq!(b.username, "x");
        assert_eq!(b.view_distance, Some(4));
    }

    #[test]
    fn builder_options_defaults() {
        let b = Bot::builder();
        // options.* defaults — nothing merged into ClientInformation until
        // a wire-field option is set; upstream's stored defaults for the rest.
        assert!(b.client_information.is_none());
        assert!(b.brand.is_none());
        assert_eq!(b.difficulty, 2);
        assert_eq!(b.chat_length_limit, 256);
        assert!(b.default_chat_patterns);
        assert_eq!(b.check_timeout_interval, Duration::from_secs(30));
        assert!(b.proxy.is_none());
    }

    #[test]
    fn builder_options_merge_into_client_information() {
        let b = Bot::builder()
            .chat(ChatVisibility::Hidden)
            .colors_enabled(false)
            .main_hand(HumanoidArm::Left)
            .enable_text_filtering(true)
            .enable_server_listing(true)
            .particle_status(ParticleStatus::Minimal)
            .skin_parts(ModelCustomization {
                cape: false,
                ..ModelCustomization::default()
            });
        let info = b.client_information.expect("ci options should merge");
        assert_eq!(info.chat_visibility, ChatVisibility::Hidden);
        assert!(!info.chat_colors);
        assert_eq!(info.main_hand, HumanoidArm::Left);
        assert!(info.text_filtering_enabled);
        assert!(info.allows_listing);
        assert_eq!(info.particle_status, ParticleStatus::Minimal);
        assert!(!info.model_customization.cape);
        assert!(info.model_customization.jacket);
        // untouched fields keep azalea's defaults (documented divergence
        // from upstream for view_distance)
        assert_eq!(info.view_distance, 8);
        assert_eq!(info.language, "en_us");
    }

    #[test]
    fn builder_stored_options() {
        let b = Bot::builder()
            .brand("mineflayer-rs")
            .difficulty(3)
            .chat_length_limit(100)
            .default_chat_patterns(false)
            .check_timeout_interval(Duration::from_secs(10));
        assert_eq!(b.brand.as_deref(), Some("mineflayer-rs"));
        assert_eq!(b.difficulty, 3);
        assert_eq!(b.chat_length_limit, 100);
        assert!(!b.default_chat_patterns);
        assert_eq!(b.check_timeout_interval, Duration::from_secs(10));
    }

    #[test]
    fn bot_event_payloads_exist() {
        // Proves the event variants the lifecycle bridge needs are real and
        // carry the planned payload types.
        let _ = BotEvent::Init;
        let _ = BotEvent::Spawn;
        let _ = BotEvent::Respawn;
        let _ = BotEvent::Death;
        let _ = BotEvent::End("socketClosed".to_string());
        let _ = BotEvent::Error("boom".to_string());
        let _ = BotEvent::Tick;
        let _ = BotEvent::ChunkColumnLoad(ChunkPos::new(0, 0));
        let _ = |p: azalea::chat::ChatPacket| BotEvent::Chat(p);
        let _ = |p: Arc<ClientboundGamePacket>| BotEvent::Packet(p);
        let _ = |i: PlayerInfo| BotEvent::PlayerJoined(i);
    }
}
