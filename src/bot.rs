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
    EntityRef, Vec3,
    auto_reconnect::AutoReconnectDelay,
    core::{
        position::{BlockPos, ChunkPos},
        registry_holder::RegistryHolder,
    },
    player::PlayerInfo,
    protocol::packets::game::{
        ClientboundGamePacket, c_block_entity_data::ClientboundBlockEntityData,
        c_sound::SoundSource,
    },
    world::WorldName,
};
use std::sync::RwLock;
use tokio::sync::{broadcast, watch};

use crate::{
    Error, Result, bridge,
    chat::{ChatPatternMatch, ChatShared, MessagePosition},
    misc::{Instrument, Particle, TitleKind},
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
    /// `error` — a non-fatal error was surfaced (currently: connection
    /// failures before login).
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
        let account = match self.auth {
            Some(Auth::Offline { username }) => azalea::account::Account::offline(&username),
            Some(Auth::Microsoft { email }) => azalea::account::Account::microsoft(&email)
                .await
                .map_err(Error::Auth)?,
            Some(Auth::Account(account)) => account,
            None => azalea::account::Account::offline(&self.username),
        };

        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let address = format!("{}:{}", self.host, self.port);
        let view_distance = self.view_distance;
        std::thread::Builder::new()
            .name(format!("mineflayer-bot-{}", self.username))
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("mineflayer: build bot runtime");
                rt.block_on(async {
                    let local = tokio::task::LocalSet::new();
                    local.spawn_local(async move {
                        let _ = init_tx
                            .send(Self::connect_azalea(account, address, view_distance).await);
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
    async fn connect_azalea(
        account: azalea::account::Account,
        address: String,
        view_distance: Option<u8>,
    ) -> Result<Bot> {
        let (client, rx) = azalea::Client::join(account, address).await?;

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
        // before the first packet arrives
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(crate::misc::MiscState::default());

        // dig bookkeeping (lastDigTime / abort generations) — same rationale
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(crate::interact::InteractShared::default());

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let quit_reason = Arc::new(Mutex::new(None));
        let chat = Arc::new(Mutex::new(ChatShared::default()));
        let (alive, alive_rx) = watch::channel(());
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
            view_distance,
            alive_rx,
            bridge::BridgeDeps {
                quit_reason: quit_reason.clone(),
                chat: chat.clone(),
                entities: entities.clone(),
                windows: windows.clone(),
                window_data: window_data.clone(),
                recipes: recipes.clone(),
            },
        );
        crate::world::spawn_packet_bridge(
            client.clone(),
            events.clone(),
            world_state.clone(),
            world_alive,
        );

        Ok(Bot {
            client,
            events,
            quit_reason,
            _alive: alive,
            chat,
            world_state,
            entities,
            windows,
            window_data,
            recipes,
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
        Self {
            client,
            events: events.clone(),
            quit_reason: Arc::new(Mutex::new(None)),
            _alive: alive,
            chat: Arc::new(Mutex::new(ChatShared::default())),
            world_state: Arc::new(RwLock::new(WorldState::default())),
            entities: Arc::new(std::sync::RwLock::new(
                crate::entity::EntityTracker::default(),
            )),
            windows: Arc::new(Mutex::new(crate::windows::WindowTracker::new(events))),
            window_data: Arc::new(Mutex::new(crate::window_types::WindowDataTracker::new())),
            recipes: Arc::new(Mutex::new(crate::craft::RecipeBook::default())),
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
