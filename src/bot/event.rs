//! The [`BotEvent`] stream — every event mineflayer's `bot.on(...)` sees,
//! one enum so subscribers match exhaustively.

use std::sync::Arc;

use mineflayer_engine::{
    EntityRef, Vec3,
    core::position::{BlockPos, ChunkPos},
    player::PlayerInfo,
    protocol::packets::game::{
        ClientboundGamePacket, c_block_entity_data::ClientboundBlockEntityData,
        c_sound::SoundSource,
    },
};

use crate::{
    chat::{ChatPatternMatch, MessagePosition},
    misc::{Instrument, Particle, TitleKind},
    scoreboard::{BossBar, ScoreBoard, ScoreItem, ScoreboardSlot, Team},
    world::Block,
};
/// Events emitted by a [`crate::Bot`], mirroring mineflayer's `bot.on(...)` events.
///
/// Bridged from the engine's [`Event`](mineflayer_engine::Event) enum; variant payloads use
/// The engine's types so no data is lost in translation.
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
    /// `kicked` — the server disconnected us with a reason. The bool is
    /// upstream `kick.js`'s `loggedIn`: `true` for an in-game kick, `false`
    /// for a kick during the login phase (see api-parity.md for the
    /// login-phase caveat).
    Kicked(String, bool),
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
    Chat(mineflayer_engine::client_chat::ChatPacket),
    /// `physicsTick` — a game tick elapsed while the world was loaded.
    Tick,
    /// Raw clientbound packet escape hatch for APIs the engine doesn't lift
    /// (scoreboards, boss bars, titles, particles, …). Emitted under the engine's
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
    /// component diff is the engine equivalent). Carries the previous
    /// position, like upstream's `oldPos` argument.
    Move {
        /// The bot's position before this move.
        old_pos: mineflayer_engine::Vec3,
    },
    /// `forcedMove` — the server placed us (teleport, spawn).
    ForcedMove,
    /// `abilities` — the server sent new player abilities; the payload is
    /// what the `PlayerAbilities` component now holds.
    Abilities(mineflayer_engine::entity::PlayerAbilities),
    /// `whisper` — an incoming whisper addressed to the bot
    /// (`ChatPacket::is_whisper` heuristic; upstream derives this event
    /// from its deprecated default chat patterns instead).
    Whisper(mineflayer_engine::client_chat::ChatPacket),
    /// `message` — any incoming message (chat, system, or game_info
    /// position). The upstream `sender` UUID is on the packet
    /// (`sender_uuid()`); upstream's `verified` flag has no the engine analog.
    Message {
        /// The packet the message arrived in.
        packet: mineflayer_engine::client_chat::ChatPacket,
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
        packet: mineflayer_engine::client_chat::ChatPacket,
    },
    /// `actionBar` — game_info/overlay text, from mc26's dedicated
    /// `c_set_action_bar_text` packet or an overlay system-chat packet.
    ActionBar(mineflayer_engine::FormattedText),
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
        /// The objective this slot showed before (`None` for an empty or
        /// dangling slot) — upstream's `previousBoard` argument.
        previous_board: Option<ScoreBoard>,
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
    /// (the engine always resolves the registry id to a name); the variant
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
    /// pushes produce this event: the engine's `Event::Packet` wraps
    /// `ClientboundGamePacket`, so config-phase pushes (server.properties /
    /// proxy packs — the dominant modern path) never reach it. The engine's
    /// `AcceptResourcePacksPlugin` also auto-accepts every push (see
    /// [`crate::Bot::deny_resource_pack`]/api-parity.md).
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
    /// reported. The engine applies updates to the world itself on the next
    /// tick, so `block_at` may briefly still return `old`.
    ///
    /// mineflayer's position-filtered `blockUpdate:x,y,z` is spelled by
    /// matching `new.pos` on this variant.
    BlockUpdate { old: Option<Block>, new: Block },
    /// `blockEntityData` — raw block-entity NBT. The engine doesn't store
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
    EntitySpawn(mineflayer_engine::EntityRef),
    /// `entityGone` — an entity was removed from the world. `entity` is the
    /// last-seen handle (`None` if the entity was never resolved); `id`
    /// always carries the protocol id since the handle can no longer report it.
    EntityGone {
        /// Last-seen handle for the removed entity, if known.
        entity: Option<mineflayer_engine::EntityRef>,
        /// The removed entity's protocol id.
        id: mineflayer_engine::core::entity_id::MinecraftEntityId,
    },
    /// `entityMoved` — an entity's position, look, or head rotation changed.
    EntityMoved(mineflayer_engine::EntityRef),
    /// `entityUpdate` — an entity's metadata changed.
    EntityUpdate(mineflayer_engine::EntityRef),
    /// `entityAttributes` — an entity's attributes were updated.
    EntityAttributes(mineflayer_engine::EntityRef),
    /// `entitySwingArm` — an entity swung its (main-hand) arm.
    EntitySwingArm(mineflayer_engine::EntityRef),
    /// `entityHurt` — an entity took damage (hurt animation, entity status,
    /// or `c_damage_event`; `source` names the responsible entity when the
    /// server sent one).
    EntityHurt {
        /// The entity that was hurt.
        entity: mineflayer_engine::EntityRef,
        /// The entity that caused the damage, if the server identified one.
        source: Option<mineflayer_engine::EntityRef>,
    },
    /// `entityDead` — an entity died (entity-event status 3).
    EntityDead(mineflayer_engine::EntityRef),
    /// `entityTaming` — tameable entity is being tamed (status 6).
    EntityTaming(mineflayer_engine::EntityRef),
    /// `entityTamed` — entity was tamed (status 7).
    EntityTamed(mineflayer_engine::EntityRef),
    /// `entityShakingOffWater` — entity shook off water (status 8).
    EntityShakingOffWater(mineflayer_engine::EntityRef),
    /// `entityEatingGrass` — entity ate grass (status 10).
    EntityEatingGrass(mineflayer_engine::EntityRef),
    /// `entityHandSwap` — entity swapped hands (status 55); tracked equipment
    /// is swapped too, like upstream.
    EntityHandSwap(mineflayer_engine::EntityRef),
    /// `entityWake` — entity woke up (animation 2).
    EntityWake(mineflayer_engine::EntityRef),
    /// `entityEat` — upstream maps animation 3 to `entityEat`; see
    /// api-parity.md for the off-hand-swing caveat.
    EntityEat(mineflayer_engine::EntityRef),
    /// `entityCriticalEffect` — critical hit particles (animation 4).
    EntityCriticalEffect(mineflayer_engine::EntityRef),
    /// `entityMagicCriticalEffect` — magic critical particles (animation 5).
    EntityMagicCriticalEffect(mineflayer_engine::EntityRef),
    /// `entityCrouch` — a metadata packet flagged the entity as sneaking
    /// (fires per packet carrying the flag, like upstream).
    EntityCrouch(mineflayer_engine::EntityRef),
    /// `entityUncrouch` — entity stopped sneaking (transition only).
    EntityUncrouch(mineflayer_engine::EntityRef),
    /// `entitySleep` — a metadata packet flagged the entity as sleeping
    /// (fires per packet carrying the flag, like upstream).
    EntitySleep(mineflayer_engine::EntityRef),
    /// `entityElytraFlew` — entity started elytra flight (metadata edge).
    EntityElytraFlew(mineflayer_engine::EntityRef),
    /// `entityEquip` — entity equipment changed (`c_set_equipment`); read the
    /// slots back via [`crate::entity::EntityExt`] / `Bot::equipment`.
    EntityEquip(mineflayer_engine::EntityRef),
    /// `entityAttach` — entity was attached to a vehicle/holder
    /// (`c_set_entity_link`, e.g. leashes).
    EntityAttach {
        /// The attached entity.
        entity: mineflayer_engine::EntityRef,
        /// The vehicle/holder entity, if resolvable.
        vehicle: Option<mineflayer_engine::EntityRef>,
    },
    /// `entityDetach` — entity was detached (`vehicle` is the previous
    /// holder, like upstream).
    EntityDetach {
        /// The detached entity.
        entity: mineflayer_engine::EntityRef,
        /// The vehicle/holder it was detached from, if known.
        vehicle: Option<mineflayer_engine::EntityRef>,
    },
    /// `entityEffect` — a mob effect was applied (`c_update_mob_effect`).
    EntityEffect {
        /// The affected entity.
        entity: mineflayer_engine::EntityRef,
        /// The effect kind.
        effect: mineflayer_engine::registry::builtin::MobEffect,
        /// Amplifier/duration/flags, mirroring upstream's
        /// `{id, amplifier, duration}` payload.
        data: mineflayer_engine::entity::MobEffectData,
    },
    /// `entityEffectEnd` — a mob effect was removed (`c_remove_mob_effect`).
    EntityEffectEnd {
        /// The affected entity.
        entity: mineflayer_engine::EntityRef,
        /// The effect kind that ended.
        effect: mineflayer_engine::registry::builtin::MobEffect,
        /// The effect's tracked parameters (upstream emits the stored
        /// effect object); `amplifier`/`duration` are `-1` when the effect
        /// was never seen, mirroring upstream's unknown-effect fallback.
        data: mineflayer_engine::entity::MobEffectData,
    },
    /// `itemDrop` — an entity's metadata carried an item stack (fires for
    /// item entities when their stack metadata arrives).
    ItemDrop(mineflayer_engine::EntityRef),
    /// `playerCollect` — an entity collected another (`c_take_item_entity`);
    /// `collected` is usually an item entity.
    PlayerCollect {
        /// The collecting entity (usually a player).
        collector: Option<mineflayer_engine::EntityRef>,
        /// The collected entity.
        collected: Option<mineflayer_engine::EntityRef>,
    },
    /// `usedFirework` — a firework rocket attached to us was registered
    /// (also feeds `bot.fireworkRocketDuration`).
    UsedFirework(mineflayer_engine::core::entity_id::MinecraftEntityId),
    /// `mount` — we started riding an entity (`c_set_passengers`).
    Mounted {
        /// The vehicle entity, if resolvable.
        vehicle: Option<mineflayer_engine::EntityRef>,
    },
    /// `dismount` — we stopped riding (`vehicle` is the previous vehicle,
    /// like upstream; also emitted when the vehicle entity goes away).
    Dismounted {
        /// The vehicle we were riding.
        vehicle: Option<mineflayer_engine::EntityRef>,
    },
    /// `diggingCompleted` — a [`crate::Bot::dig`] finished; the payload is the new
    /// (usually air) block at the dug position, like upstream's `newBlock`.
    DiggingCompleted(Block),
    /// `diggingAborted` — a [`crate::Bot::dig`] ended without breaking the block.
    DiggingAborted {
        /// The block that was being dug.
        block: Block,
        /// Why the dig ended.
        reason: crate::interact::DigAbortReason,
    },
    /// `blockPlaced` — the server confirmed a [`crate::Bot::place_block`].
    BlockPlaced {
        /// The block previously at the position, if observed.
        old: Option<Block>,
        /// The block the server placed.
        new: Block,
    },
    /// `entityPlaced` (undocumented upstream) — the entity a
    /// [`crate::Bot::place_entity`] spawned.
    EntityPlaced(mineflayer_engine::EntityRef),
    /// `goal_updated` — `pathfinder.set_goal` installed a new goal
    /// (upstream emits `(goal, dynamic)`).
    GoalUpdated {
        /// The goal that was installed.
        goal: crate::pathfinder::Goal,
        /// The `set_goal` `dynamic` argument it was installed with.
        dynamic: bool,
    },
    /// `path_update` — a new path was found, or a calculation finished
    /// (the engine also ends a failed calculation this way, matching
    /// upstream's per-result `path_update`; `path` is empty then).
    /// `is_partial` mirrors upstream's `r.partial` flag and `status`
    /// upstream's `results.status` (`'success'`/`'partial'`/`'noPath'`/
    /// `'timeout'`).
    PathUpdate {
        /// The path node list (empty while no path is executing).
        path: Vec<BlockPos>,
        /// Whether the path only gets partway to the goal.
        is_partial: bool,
        /// The search outcome that produced this update.
        status: crate::pathfinder::PathStatus,
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
    /// the facade can observe. The engine's internal repaths (stuck, block
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
    /// `setWindowItems:{windowId}` — a `container_set_content` packet's
    /// full slot list was applied to a resolved window (0 = player
    /// inventory or the open container). Emits after that window's
    /// per-slot [`BotEvent::WindowUpdateSlot`]s and
    /// [`BotEvent::HeldItemChanged`], and before [`BotEvent::WindowOpen`]
    /// on a window's first fill — the upstream ordering that
    /// `syncWindow`/`prepareWindow` gate on. Not emitted for slot data
    /// buffered ahead of `open_screen` or remapped to a closed window's
    /// player region (upstream returns before emitting on both paths).
    /// mc26 has no `transaction` wire — a click the server rejects comes
    /// back as one of these corrective resyncs.
    SetWindowItems {
        /// Protocol window id (0 = the player inventory).
        window_id: i32,
    },
    /// mineflayer-auto-eat `eatStart` — the bot started eating `food`
    /// (in the offhand when `offhand`). Upstream emits the sanitized
    /// opts; ours carries the resolved food and hand. Upstream fires it
    /// on `bot.autoEat`; plugin events ride the shared stream here, same
    /// as the collect/pvp variants above.
    EatStart {
        /// The stack being eaten.
        food: crate::windows::Item,
        /// Whether the offhand is doing the eating.
        offhand: bool,
        /// Whether the previously held item is re-equipped after the eat
        /// (upstream's sanitized `equipOldItem` — resolved from
        /// `EatOpts.equip_old_item` or `opts.returnToLastItem`).
        equip_old_item: bool,
    },
    /// mineflayer-auto-eat `eatFail` — the eat failed; `0` is the
    /// upstream `Error`'s message.
    EatFail(String),
    /// mineflayer-auto-eat `eatFinish` — the eat attempt concluded.
    /// Upstream emits it in `finally`, so it follows `EatFail` too —
    /// it is a "stopped eating" signal, not a success signal.
    EatFinish {
        /// The stack that was eaten (or attempted).
        food: crate::windows::Item,
        /// Whether the offhand was doing the eating.
        offhand: bool,
        /// Whether the previously held item was re-equipped (upstream's
        /// sanitized `equipOldItem`).
        equip_old_item: bool,
    },
    /// `connect` — the connection is live. Upstream fires on raw socket
    /// connect (pre-login); the engine surfaces no socket-level event, so this
    /// is emitted right before the first [`BotEvent::Login`] (the
    /// clientbound login packet is the earliest proof the wire is up).
    /// Once per connection — later `Login`s (world switches) don't refire
    /// it.
    Connected,
    /// mineflayer-pvp `startedAttacking` — `pvp().attack()` engaged a
    /// target. Fires on every successful `attack` call including
    /// retargets; upstream emits `stoppedAttacking` + `startedAttacking`
    /// on a retarget — ours keeps the one run alive and fires only this
    /// (see the `StoppedAttacking` row in api-parity.md).
    StartedAttacking,
    /// mineflayer-pvp `attackedTarget` — the pvp driver swung at the
    /// target this tick. No payload, like upstream.
    AttackedTarget,
    /// mineflayer-collectblock `collectBlock_finished` — a `collect`
    /// call concluded, successfully or not (upstream emits it from a
    /// `finally`). [`BotEvent::CollectDone`] stays the additive
    /// per-run success report; a cancelled collect emits this without
    /// it.
    CollectBlockFinished,
}
