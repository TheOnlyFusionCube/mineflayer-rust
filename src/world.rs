//! world — `prismarine-world`/`prismarine-block` analog over azalea's world
//! storage, plus the facade-kept world/game state (time, weather, spawn
//! point, difficulty) that azalea does not track and which is therefore
//! recovered here from raw `ClientboundGamePacket`s.

use std::{collections::HashMap, io::Cursor, sync::RwLock};

use azalea::{
    Event,
    block::{BlockState, BlockStates, BlockTrait},
    buf::AzBuf,
    core::{
        data_registry::DataRegistryWithKey,
        difficulty::Difficulty,
        game_type::GameMode,
        hit_result::HitResult,
        position::{BlockPos, ChunkPos, GlobalPos, Vec3},
    },
    entity::{LookDirection, Position, dimensions::EntityDimensions},
    interact::pick::HitResultComponent,
    local_player::WorldHolder,
    physics::{clip::traverse_blocks, collision::BlockWithShape},
    protocol::packets::game::{
        ClientboundGamePacket, c_block_event::ClientboundBlockEvent, c_game_event::EventType,
        c_set_time::ClockState, s_sign_update::ServerboundSignUpdate,
    },
    registry::{DataRegistryKey, builtin::BlockKind, identifier::Identifier},
    world::{World, WorldName, find_blocks::find_blocks_in_chunk, iterators::ChunkIterator},
};

use crate::{Bot, BotEvent, Error, Result};

const DEFAULT_MAX_DISTANCE: f64 = 16.;
const DEFAULT_COUNT: usize = 1;
/// mineflayer's `blockAtCursor` default max distance (256 blocks).
pub const DEFAULT_CURSOR_DISTANCE: f64 = 256.;
/// mineflayer's `entityAtCursor` default max distance (3.5 blocks).
pub const DEFAULT_ENTITY_CURSOR_DISTANCE: f64 = 3.5;

/// The minecraft world the bot is in — the `prismarine-world` analog.
///
/// Wraps the azalea [`Client`](azalea::Client), which owns the shared
/// `azalea_world::World`; reach the raw world handle through
/// [`WorldRef::client`] (`client.world()` / `client.partial_world()`).
/// `None`-returning methods simply indicate the chunk (or world) isn't
/// loaded.
#[derive(Clone)]
pub struct WorldRef {
    client: azalea::Client,
}

impl std::fmt::Debug for WorldRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorldRef")
    }
}

impl WorldRef {
    pub(crate) fn new(client: azalea::Client) -> Self {
        Self { client }
    }

    /// The azalea client that owns the underlying `azalea_world::World`.
    pub fn client(&self) -> &azalea::Client {
        &self.client
    }

    /// `world.getBlock` — block state, `None` when the chunk isn't loaded.
    pub fn get_block_state(&self, pos: BlockPos) -> Option<BlockState> {
        with_world(&self.client, |w| w.get_block_state(pos)).flatten()
    }

    /// `world.getBlock` as a [`Block`] snapshot.
    pub fn block_at(&self, pos: BlockPos) -> Option<Block> {
        self.get_block_state(pos)
            .map(|state| Block::new(&self.client, pos, state))
    }

    /// `world.raycast` — walks the block grid along the segment. With a
    /// `matcher` the first voxel it accepts wins; with `None` the first
    /// block whose collision shape is intersected wins.
    pub fn raycast(
        &self,
        from: Vec3,
        direction: Vec3,
        range: f64,
        matcher: Option<&BlockMatcher>,
    ) -> Option<Block> {
        let pred: Option<Box<dyn Fn(BlockPos, BlockState) -> bool + '_>> = matcher.map(|m| {
            Box::new(move |pos: BlockPos, state: BlockState| {
                m.matches(&Block::new(&self.client, pos, state))
            }) as _
        });
        with_world(&self.client, |w| {
            raycast_blocks(w, from, direction, range, pred.as_deref())
                .map(|(pos, state)| Block::new(&self.client, pos, state))
        })
        .flatten()
    }

    /// Client-side world write (e.g. predicting a dig); returns the replaced
    /// state. `None` when the chunk isn't loaded.
    pub fn set_block_state(&self, pos: BlockPos, state: BlockState) -> Option<BlockState> {
        with_world(&self.client, |w| w.set_block_state(pos, state)).flatten()
    }

    /// `world.findBlocks` — same engine as [`Bot::find_blocks`]. A `None`
    /// `point` centers on the client entity's position — upstream's
    /// `options.point || bot.entity.position` (a `WorldRef` is always a
    /// view onto a bot's client). Pre-spawn falls back to the origin.
    pub fn find_blocks(&self, opts: &FindBlocksOpts) -> Vec<BlockPos> {
        let point = opts.point.map_or_else(
            || {
                self.client
                    .get_component::<Position>()
                    .map(|p| (**p).to_block_pos_floor())
                    .unwrap_or_default()
            },
            |p| p.to_block_pos_floor(),
        );
        // upstream's `options.maxDistance || 16` / `options.count || 1`
        // coerce falsy: `0` (and `NaN`) take the default just like an
        // unset option does.
        let max_distance = opts
            .max_distance
            .filter(|d| *d != 0. && !d.is_nan())
            .unwrap_or(DEFAULT_MAX_DISTANCE);
        let count = opts.count.filter(|c| *c != 0).unwrap_or(DEFAULT_COUNT);
        with_world(&self.client, |w| match &opts.matching {
            BlockMatcher::States(states) => find_states(w, point, states, max_distance, count),
            BlockMatcher::Predicate(pred) => find_matching(
                w,
                point,
                &|pos, state| {
                    euclidean(pos, point) <= max_distance
                        && pred(&Block::new(&self.client, pos, state))
                },
                max_distance,
                count,
            ),
        })
        .unwrap_or_default()
    }
}

/// A snapshot of a block in the world — the `prismarine-block` analog.
///
/// `state` is the block state as last observed; re-read it through
/// `block.world` if the block may have changed since. Digging,
/// interaction and pathfinding units (U6/U8/U10) consume the fields
/// below: `pos` + `kind`/`state_id`/`properties` for matching,
/// `behavior` for dig speed and `world` for live re-reads and
/// client-side predictions.
#[derive(Clone)]
pub struct Block {
    /// Position in the world.
    pub pos: BlockPos,
    /// The observed block state (kind + state properties).
    pub state: BlockState,
    /// The world this block was read in.
    pub world: WorldRef,
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Block")
            .field("pos", &self.pos)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Block {
    pub(crate) fn new(client: &azalea::Client, pos: BlockPos, state: BlockState) -> Self {
        Self {
            pos,
            state,
            world: WorldRef::new(client.clone()),
        }
    }

    /// `block.name` — e.g. `"minecraft:grass_block"`. Diverges from
    /// upstream, whose `block.name` strips the namespace
    /// (`"grass_block"`); compare with [`crate::Item::name`], which
    /// does strip it.
    pub fn name(&self) -> &'static str {
        self.state.as_block_kind().to_str()
    }

    /// `block.kind` — the block type, e.g. [`BlockKind::GrassBlock`].
    pub fn kind(&self) -> BlockKind {
        self.state.as_block_kind()
    }

    /// `block.stateId` — the protocol block-state id.
    pub fn state_id(&self) -> u32 {
        self.state.id() as u32
    }

    /// `block.position` as a `Vec3` (the block's corner).
    pub fn position(&self) -> Vec3 {
        Vec3 {
            x: self.pos.x as f64,
            y: self.pos.y as f64,
            z: self.pos.z as f64,
        }
    }

    /// `block.boundingBox` — `"block"` when the outline shape is non-empty.
    pub fn bounding_box(&self) -> &'static str {
        if self.state.outline_shape().is_empty() {
            "empty"
        } else {
            "block"
        }
    }

    /// `block.diggable` — false for bedrock-like indestructible blocks.
    pub fn diggable(&self) -> bool {
        self.behavior().destroy_time >= 0.
    }

    /// `block.material`/`block.harvestTools` backing data — azalea's
    /// `BlockBehavior` (friction, destroy time, …). U6 uses `destroy_time`,
    /// `requires_correct_tool_for_drops` and friends for dig timing.
    pub fn behavior(&self) -> azalea::block::BlockBehavior {
        Box::<dyn BlockTrait>::from(self.state).behavior()
    }

    /// `block.getProperty(name)` — e.g. `property("facing")` → `"north"`.
    pub fn property(&self, name: &str) -> Option<&'static str> {
        Box::<dyn BlockTrait>::from(self.state).get_property(name)
    }

    /// `block.getProperties()` — all state properties keyed by name.
    pub fn properties(&self) -> HashMap<String, &'static str> {
        Box::<dyn BlockTrait>::from(self.state)
            .property_map()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }
}

/// `matching` argument for [`Bot::find_blocks`]/[`Bot::block_at_cursor`] —
/// either a set of block kinds/states or an arbitrary predicate.
pub enum BlockMatcher {
    /// Fast path: match any of these block states (palette-filtered).
    States(BlockStates),
    /// Arbitrary predicate over the resolved [`Block`].
    Predicate(Box<dyn Fn(&Block) -> bool + Send + Sync>),
}

impl BlockMatcher {
    fn matches(&self, block: &Block) -> bool {
        match self {
            Self::States(states) => states.contains(&block.state),
            Self::Predicate(pred) => pred(block),
        }
    }
}

impl From<BlockKind> for BlockMatcher {
    fn from(kind: BlockKind) -> Self {
        Self::States(BlockStates::from([kind]))
    }
}
impl From<BlockState> for BlockMatcher {
    fn from(state: BlockState) -> Self {
        Self::States(BlockStates::from(state.id()..=state.id()))
    }
}
impl From<Vec<BlockKind>> for BlockMatcher {
    fn from(kinds: Vec<BlockKind>) -> Self {
        Self::States(BlockStates::from(kinds.as_slice()))
    }
}
impl<const N: usize> From<[BlockKind; N]> for BlockMatcher {
    fn from(kinds: [BlockKind; N]) -> Self {
        Self::States(BlockStates::from(kinds))
    }
}
impl From<&[BlockKind]> for BlockMatcher {
    fn from(kinds: &[BlockKind]) -> Self {
        Self::States(BlockStates::from(kinds))
    }
}
impl<F> From<F> for BlockMatcher
where
    F: Fn(&Block) -> bool + Send + Sync + 'static,
{
    fn from(pred: F) -> Self {
        Self::Predicate(Box::new(pred))
    }
}

/// `findBlocks` options — defaults: `point` = bot position (floored),
/// `max_distance` = 16, `count` = 1. Upstream's `||` defaults coerce
/// falsy, so `0` (and `NaN` for `max_distance`) take the default too.
pub struct FindBlocksOpts {
    /// Which blocks to match.
    pub matching: BlockMatcher,
    /// Center of the search; defaults to the bot's position (floored).
    pub point: Option<Vec3>,
    /// Euclidean distance bound; default 16 (also for `0.`/`NaN`).
    pub max_distance: Option<f64>,
    /// Maximum results (closest-first); default 1 (also for `0`).
    pub count: Option<usize>,
}

impl FindBlocksOpts {
    pub fn new(matching: impl Into<BlockMatcher>) -> Self {
        Self {
            matching: matching.into(),
            point: None,
            max_distance: None,
            count: None,
        }
    }
    pub fn point(mut self, point: Vec3) -> Self {
        self.point = Some(point);
        self
    }
    pub fn max_distance(mut self, max_distance: f64) -> Self {
        self.max_distance = Some(max_distance);
        self
    }
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }
}

/// `Location` (module export) — upstream's legacy helper from
/// `lib/location.js`: splits an absolute position into its floored block
/// position, the point inside its 16³ chunk section, and the packed
/// block/biome indices chunk consumers index with.
///
/// Upstream's fields are `Vec3`s; here the integer ones are [`BlockPos`]
/// and the indices are plain ints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    /// `floored` — the absolute block position (each component floored).
    pub floored: BlockPos,
    /// `blockPoint` — the position inside the chunk section (0..=15 each,
    /// euclidean-mod 16 like upstream's `modulus`, so negative coordinates
    /// wrap correctly).
    pub block_point: BlockPos,
    /// `chunkCorner` — `floored - block_point` (all components multiples
    /// of 16).
    pub chunk_corner: BlockPos,
    /// `blockIndex` — `block_point.x + 16 * z + 256 * y` (upstream's
    /// packed index into a chunk section's block array).
    pub block_index: u32,
    /// `biomeBlockIndex` — `block_point.x + 16 * z`.
    pub biome_block_index: u32,
    /// `chunkYIndex` — `floor(absolute.y / 16)` on the *unfloored* y,
    /// matching upstream exactly.
    pub chunk_y_index: i32,
}

impl Location {
    /// `new Location(absoluteVector)`.
    pub fn new(absolute: Vec3) -> Self {
        let floored = BlockPos::new(
            absolute.x.floor() as i32,
            absolute.y.floor() as i32,
            absolute.z.floor() as i32,
        );
        let block_point = BlockPos::new(
            floored.x.rem_euclid(16),
            floored.y.rem_euclid(16),
            floored.z.rem_euclid(16),
        );
        Self {
            floored,
            block_point,
            chunk_corner: floored - block_point,
            block_index: (block_point.x + 16 * block_point.z + 256 * block_point.y) as u32,
            biome_block_index: (block_point.x + 16 * block_point.z) as u32,
            chunk_y_index: (absolute.y / 16.).floor() as i32,
        }
    }
}

/// `bot.time` — everything derived from the server's `set_time` clocks.
/// `None` from [`Bot::time`] until the first `set_time` packet arrives.
#[derive(Clone, Debug, PartialEq)]
pub struct Time {
    /// Whether the daylight cycle is running (clock `rate > 0`).
    pub do_daylight_cycle: bool,
    /// The dimension clock's total ticks. Same value as `time`; upstream
    /// keeps a `BigInt`/`Number` split we don't need (`u64` covers both).
    pub big_time: u64,
    /// Total ticks on the dimension clock.
    pub time: u64,
    /// `time % 24000`.
    pub time_of_day: u64,
    /// `time / 24000`.
    pub day: u64,
    /// `time_of_day < 13000`.
    pub is_day: bool,
    /// `day % 8`.
    pub moon_phase: u64,
    /// The world's age in ticks (monotonic, shared across dimensions).
    pub big_age: u64,
    /// Same as `big_age`.
    pub age: u64,
}

/// `bot.game` — login/respawn/game-event metadata.
#[derive(Clone, Debug)]
pub struct Game {
    /// `"flat"` or `"default"` (from `is_flat` on login/respawn); `None`
    /// until the login packet was seen.
    pub level_type: Option<&'static str>,
    /// `bot.game.dimension` — the current dimension identifier.
    pub dimension: Option<WorldName>,
    /// `bot.game.difficulty`.
    pub difficulty: Option<Difficulty>,
    /// `bot.game.gameMode`.
    pub game_mode: Option<GameMode>,
    /// `bot.game.hardcore`.
    pub hardcore: Option<bool>,
    /// `bot.game.maxPlayers`.
    pub max_players: Option<i32>,
    /// `bot.game.serverBrand` (from the `minecraft:brand` plugin channel).
    pub server_brand: Option<String>,
    /// `bot.game.minY` — lowest buildable y.
    pub min_y: Option<i32>,
    /// `bot.game.height` — world build height.
    pub height: Option<u32>,
}

/// Facade-kept world/game state, fed by [`observe_event`] inside the
/// bridge's event loop. Fields that azalea already owns (dimension,
/// gamemode) are read live off the client instead.
#[derive(Default)]
pub(crate) struct WorldState {
    /// `bot.spawnPoint` — from `c_set_default_spawn_position` (dropped
    /// by azalea).
    pub spawn_point: Option<GlobalPos>,
    /// Per-dimension clocks from `c_set_time`, advanced one tick per
    /// game tick like vanilla does.
    pub clocks: HashMap<Identifier, ClockState>,
    /// `game_time` from `c_set_time` — the world's age.
    pub game_time: u64,
    pub is_raining: bool,
    pub rain_state: f32,
    pub thunder_state: f32,
    pub difficulty: Option<Difficulty>,
    pub hardcore: Option<bool>,
    pub max_players: Option<i32>,
    pub is_flat: Option<bool>,
    pub server_brand: Option<String>,
    /// `BlockState` last *reported* per updated position — the
    /// `blockUpdate` `oldBlock` source of truth. The bridge decodes
    /// `Event::Packet` only after azalea's `Update` pass finished (its
    /// `run_schedule_loop` holds the ECS write guard across the whole
    /// pass, and both `packet_listener` and
    /// `handle_block_update_event` run inside it — the async receiver
    /// can't interleave mid-pass), so the live world already shows the
    /// new state when we read `old`. Positions never seen updated fall
    /// back to that post-apply read (they report `old == new` — the
    /// best available without shadowing every loaded block). Cleared
    /// on login/respawn (dimension change) and per-position on chunk
    /// unload, bounded by [`MAX_TRACKED_UPDATES`].
    last_block_updates: HashMap<BlockPos, BlockState>,
}

/// Bound on [`WorldState::last_block_updates`] — generous enough for a
/// busy redstone world; a full map clears wholesale on the next *new*
/// position (already-tracked positions keep their history — see
/// [`WorldState::update_and_old`]).
const MAX_TRACKED_UPDATES: usize = 1 << 16;

impl WorldState {
    /// Record `new_state` as `pos`'s last-known state and return the
    /// previously recorded one — the `blockUpdate` `oldBlock` source of
    /// truth (see the field doc for why the live world can't answer).
    /// `None` means we never saw this position updated; the caller
    /// falls back to the post-apply world read.
    fn update_and_old(&mut self, pos: BlockPos, new_state: BlockState) -> Option<BlockState> {
        // Only a *new* position can grow the map — re-updates to a
        // tracked position must keep their history even at capacity.
        if self.last_block_updates.len() >= MAX_TRACKED_UPDATES
            && !self.last_block_updates.contains_key(&pos)
        {
            self.last_block_updates.clear();
        }
        self.last_block_updates.insert(pos, new_state)
    }

    /// Vanilla clock tick: `partial_tick += rate; total_ticks += floor`.
    fn tick_clocks(&mut self) {
        for clock in self.clocks.values_mut() {
            clock.partial_tick += clock.rate;
            let full = clock.partial_tick.floor();
            clock.partial_tick -= full;
            clock.total_ticks = clock.total_ticks.wrapping_add(full as u64);
        }
    }

    /// Port of mineflayer's rain.js `game_state_change` handling.
    fn apply_game_event(&mut self, event: EventType, param: f32) -> Vec<BotEvent> {
        let mut out = vec![];
        match event {
            EventType::StartRaining => self.set_raining(true, &mut out),
            EventType::StopRaining => self.set_raining(false, &mut out),
            EventType::RainLevelChange => {
                let was_wet = self.rain_state > 0.;
                self.rain_state = param;
                out.push(BotEvent::WeatherUpdate);
                if was_wet != (param > 0.) {
                    self.set_raining(param > 0., &mut out);
                }
            }
            EventType::ThunderLevelChange => {
                self.thunder_state = param;
                out.push(BotEvent::WeatherUpdate);
            }
            EventType::ChangeGameMode => out.push(BotEvent::GameStateChanged),
            _ => {}
        }
        out
    }

    fn set_raining(&mut self, is_raining: bool, out: &mut Vec<BotEvent>) {
        if self.is_raining == is_raining {
            return;
        }
        self.is_raining = is_raining;
        out.push(BotEvent::Rain);
    }
}

impl Time {
    /// `bot.time` resolution: the dimension's clock, with the overworld
    /// clock as fallback for dimensions that share it (like the nether).
    fn compute(state: &WorldState, dimension: Option<&WorldName>) -> Option<Self> {
        let clock = dimension
            .map(|d| state.clocks.get(&d.0))
            .unwrap_or_else(|| state.clocks.get(&Identifier::from("minecraft:overworld")))
            .or_else(|| state.clocks.values().next())?;
        let day = clock.total_ticks / 24000;
        let time_of_day = clock.total_ticks % 24000;
        Some(Time {
            do_daylight_cycle: clock.rate > 0.,
            big_time: clock.total_ticks,
            time: clock.total_ticks,
            time_of_day,
            day,
            is_day: time_of_day < 13000,
            moon_phase: day % 8,
            big_age: state.game_time,
            age: state.game_time,
        })
    }
}

fn euclidean(a: BlockPos, b: BlockPos) -> f64 {
    a.distance_to(b)
}

/// `world.getBlock` via the client's shared world, without naming the
/// lock type (parking_lot is not a direct dependency).
fn with_world<R>(client: &azalea::Client, f: impl FnOnce(&World) -> R) -> Option<R> {
    let holder = client.get_component::<WorldHolder>()?;
    let world = holder.shared.read();
    Some(f(&world))
}

fn block_at(client: &azalea::Client, pos: BlockPos) -> Option<Block> {
    with_world(client, |w| w.get_block_state(pos))
        .flatten()
        .map(|state| Block::new(client, pos, state))
}

/// Spiral-scan `find_blocks_in_chunk` over the chunk columns within
/// `ceil((max_distance+8)/16)` chunk layers — port of mineflayer's
/// findBlocks octahedron scan with its "stop a layer after reaching
/// count" early exit.
fn find_states(
    world: &World,
    point: BlockPos,
    states: &BlockStates,
    max_distance: f64,
    count: usize,
) -> Vec<BlockPos> {
    let mut found = vec![];
    let layers = ((max_distance + 8.) / 16.).ceil().max(1.) as u32;
    let mut iter = ChunkIterator::new(ChunkPos::from(point), layers);
    let mut started_layer = 0;
    while let Some(chunk_pos) = iter.next() {
        if let Some(chunk) = world.chunks.get(&chunk_pos) {
            find_blocks_in_chunk(
                states,
                chunk_pos,
                &chunk.read(),
                world.chunks.min_y(),
                |pos| {
                    if euclidean(pos, point) <= max_distance {
                        found.push(pos)
                    }
                },
            );
        }
        if started_layer != iter.layer && found.len() >= count {
            break;
        }
        started_layer = iter.layer;
    }
    found.sort_by(|a, b| {
        euclidean(*a, point)
            .partial_cmp(&euclidean(*b, point))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found.truncate(count);
    found
}

/// Predicate-mode find — scans every loaded section index (no palette
/// fast-path is possible for arbitrary predicates).
fn find_matching(
    world: &World,
    point: BlockPos,
    matches: &dyn Fn(BlockPos, BlockState) -> bool,
    max_distance: f64,
    count: usize,
) -> Vec<BlockPos> {
    let mut found = vec![];
    let layers = ((max_distance + 8.) / 16.).ceil().max(1.) as u32;
    let mut iter = ChunkIterator::new(ChunkPos::from(point), layers);
    let mut started_layer = 0;
    let min_y = world.chunks.min_y();
    while let Some(chunk_pos) = iter.next() {
        if let Some(chunk) = world.chunks.get(&chunk_pos) {
            let chunk = chunk.read();
            for (i, section) in chunk.sections.iter().enumerate() {
                let base_y = min_y + i as i32 * 16;
                for idx in 0..4096usize {
                    let state = section.states.get_at_index(idx);
                    let sp = section.states.pos_from_index(idx);
                    let pos = BlockPos {
                        x: chunk_pos.x * 16 + sp.x as i32,
                        y: base_y + sp.y as i32,
                        z: chunk_pos.z * 16 + sp.z as i32,
                    };
                    if euclidean(pos, point) <= max_distance && matches(pos, state) {
                        found.push(pos);
                    }
                }
            }
        }
        if started_layer != iter.layer && found.len() >= count {
            break;
        }
        started_layer = iter.layer;
    }
    found.sort_by(|a, b| {
        euclidean(*a, point)
            .partial_cmp(&euclidean(*b, point))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found.truncate(count);
    found
}

/// `world.raycast` core — Amanatides & Woo DDA via azalea's
/// `traverse_blocks`, faithful to upstream `WorldSync.raycast`: with a
/// `matcher` the first voxel it accepts wins outright (empty or partial
/// shapes the ray only grazes are reachable); with `None` the first
/// block whose *collision* shape is clipped wins (upstream's
/// `iter.intersect(block.shapes)`).
fn raycast_blocks(
    world: &World,
    from: Vec3,
    direction: Vec3,
    range: f64,
    matcher: Option<&dyn Fn(BlockPos, BlockState) -> bool>,
) -> Option<(BlockPos, BlockState)> {
    let len = direction.length();
    if range <= 0. || len == 0. {
        return None;
    }
    let to = from + direction * (range / len);
    traverse_blocks(
        from,
        to,
        (),
        |_, pos| {
            let state = world.chunks.get_block_state(pos)?;
            match matcher {
                Some(m) => m(pos, state).then_some(Some((pos, state))),
                None => state
                    .collision_shape()
                    .clip(from, to, pos)
                    .map(|_| Some((pos, state))),
            }
        },
        |_| None,
    )
}

/// `canSeeBlock`'s matcher (upstream `match` in blocks.js): the ray
/// clipping the block's *collision* shape counts, and so does merely
/// entering the target's voxel — flat/empty shapes are still "seen".
fn can_see_accepts(
    target: BlockPos,
    from: Vec3,
    to: Vec3,
    pos: BlockPos,
    state: BlockState,
) -> bool {
    pos == target || state.collision_shape().clip(from, to, pos).is_some()
}

/// mineflayer sign text validation (`< 4` lines → error; `> 45` chars per
/// line → error; shorter text pads with empty lines).
fn sign_lines(text: &str) -> Result<[String; 4]> {
    let mut lines: [String; 4] = Default::default();
    for (i, line) in text.split('\n').enumerate() {
        if i >= 4 {
            return Err(Error::SignText("too many lines for sign text".to_string()));
        }
        if line.chars().count() > 45 {
            return Err(Error::SignText(
                "signs have a maximum of 45 characters per line".to_string(),
            ));
        }
        lines[i] = line.to_string();
    }
    Ok(lines)
}

impl Bot {
    /// `bot.world` — the world facade (`getBlock`/`raycast`/`findBlocks`
    /// analog). `None` before the world is available (pre-spawn, respawn
    /// gap).
    pub fn world(&self) -> Option<WorldRef> {
        self.client
            .get_component::<WorldHolder>()
            .map(|_| WorldRef::new(self.client.clone()))
    }

    /// `bot.blockAt(point)` — `None` when the chunk isn't loaded.
    pub fn block_at(&self, pos: BlockPos) -> Option<Block> {
        self.world()?.block_at(pos)
    }

    /// `bot.findBlocks({matching, point, maxDistance, count})` — positions
    /// of matching blocks, closest first.
    pub fn find_blocks(&self, opts: FindBlocksOpts) -> Vec<BlockPos> {
        let mut opts = opts;
        if opts.point.is_none() {
            opts.point = Some(self.position());
        }
        self.world()
            .map(|w| w.find_blocks(&opts))
            .unwrap_or_default()
    }

    /// `bot.findBlock(options)` — the closest match as a [`Block`].
    pub fn find_block(&self, opts: FindBlocksOpts) -> Option<Block> {
        let pos = self.find_blocks(opts).into_iter().next()?;
        self.block_at(pos)
    }

    /// `bot.blockAtCursor(maxDistance, matcher)` — the block the bot is
    /// looking at, by outline shape. `max_distance` defaults to 256
    /// upstream; pass [`f64::INFINITY`]-ish values with care (unloaded
    /// chunks are skipped, not walked forever — the DDA still traverses
    /// them so keep it bounded).
    pub fn block_at_cursor(
        &self,
        max_distance: f64,
        matcher: Option<BlockMatcher>,
    ) -> Option<Block> {
        let (eye, look) = self.eye_and_look()?;
        self.world()?.raycast(
            eye,
            azalea::entity::view_vector(look),
            max_distance,
            matcher.as_ref(),
        )
    }

    /// Deprecated upstream; `blockAtCursor` covering the same 80 blocks
    /// (maxSteps × vectorLength = 256 × 5/16).
    #[deprecated(note = "use `block_at_cursor` instead")]
    pub fn block_in_sight(&self) -> Option<Block> {
        self.block_at_cursor(80., None)
    }

    /// `bot.entityAtCursor(maxDistance)` — divergence: azalea only exposes
    /// the crosshair hit its own `pick` plugin computes (interaction +
    /// block ranges, not an arbitrary distance), so `max_distance` acts as
    /// a cap on that result rather than driving an independent raycast.
    /// Default upstream: 3.5.
    pub fn entity_at_cursor(&self, max_distance: f64) -> Option<azalea::EntityRef> {
        // copy the hit out and drop the component guard — eye_pos /
        // entity_ref_for re-lock the ECS and nested reads deadlock behind
        // a queued writer
        let (target, location) = {
            let hit = self.client.get_component::<HitResultComponent>()?;
            let HitResult::Entity(hit) = &**hit else {
                return None;
            };
            (hit.entity, hit.location)
        };
        let eye = self.eye_pos()?;
        if location.distance_to(eye) > max_distance {
            return None;
        }
        Some(self.client.entity_ref_for(target))
    }

    /// `bot.blockAtEntityCursor(entity, maxDistance, matcher)` — ray from
    /// the entity's eyes. Divergence: upstream uses `entity.height`; we use
    /// the vanilla eye height, which is what the entity actually looks
    /// from.
    pub fn block_at_entity_cursor(
        &self,
        entity: &azalea::EntityRef,
        max_distance: f64,
        matcher: Option<BlockMatcher>,
    ) -> Option<Block> {
        let (eye, look) = entity
            .try_query_self::<(&Position, &EntityDimensions, &LookDirection), _>(|(p, d, l)| {
                (p.up(d.eye_height as f64), *l)
            })
            .ok()?;
        self.world()?.raycast(
            eye,
            azalea::entity::view_vector(look),
            max_distance,
            matcher.as_ref(),
        )
    }

    /// `bot.canSeeBlock(block)` — ray from the eyes to the block's
    /// center; the first block upstream's matcher accepts must be the
    /// target (see `can_see_accepts`).
    pub fn can_see_block(&self, block: &Block) -> bool {
        let (Some(eye), Some(world)) = (self.eye_pos(), self.world()) else {
            return false;
        };
        let target = block.pos;
        let dir = target.center() - eye;
        let len = dir.length();
        if len == 0. {
            return true;
        }
        let range = eye.distance_to(block.position());
        let to = eye + dir * (range / len);
        let matcher =
            BlockMatcher::from(move |b: &Block| can_see_accepts(target, eye, to, b.pos, b.state));
        world
            .raycast(eye, dir * (1. / len), range, Some(&matcher))
            .is_some_and(|hit| hit.pos == target)
    }

    /// `bot.updateSign(block, text, back)` — writes the sign via
    /// `ServerboundSignUpdate`. `text` is `\n`-separated, ≤ 4 lines and ≤ 45
    /// chars each (server-enforced; upstream trusts it and drops the
    /// packet after `end`).
    ///
    /// Reading sign text is a known divergence: azalea doesn't store block
    /// entities, so there is no `getSignText`-equivalent here.
    pub fn update_sign(&self, block: &Block, text: &str, back: bool) -> Result<()> {
        self.client.write_packet(ServerboundSignUpdate {
            pos: block.pos,
            is_front_text: !back,
            lines: sign_lines(text)?,
        });
        Ok(())
    }

    /// `bot.spawnPoint` — decoded from `c_set_default_spawn_position`
    /// (azalea drops it). `BlockPos::default()` until the packet arrives.
    pub fn spawn_point(&self) -> BlockPos {
        self.world_state
            .read()
            .unwrap()
            .spawn_point
            .as_ref()
            .map(|g| g.pos)
            .unwrap_or_default()
    }

    /// `bot.time` — `None` until the first `set_time` packet.
    pub fn time(&self) -> Option<Time> {
        let state = self.world_state.read().unwrap();
        Time::compute(&state, self.dimension().as_ref())
    }

    /// `bot.isRaining`.
    pub fn is_raining(&self) -> bool {
        self.world_state.read().unwrap().is_raining
    }

    /// `bot.rainState` — 0.0..1.0.
    pub fn rain_state(&self) -> f32 {
        self.world_state.read().unwrap().rain_state
    }

    /// `bot.thunderState` — 0.0..1.0.
    pub fn thunder_state(&self) -> f32 {
        self.world_state.read().unwrap().thunder_state
    }

    /// `bot.game` — snapshot; `None` fields weren't seen yet.
    pub fn game(&self) -> Game {
        let (min_y, height) = with_world(&self.client, |w| (w.chunks.min_y(), w.chunks.height()))
            .map_or((None, None), |(y, h)| (Some(y), Some(h)));
        let state = self.world_state.read().unwrap();
        Game {
            level_type: state
                .is_flat
                .map(|flat| if flat { "flat" } else { "default" }),
            dimension: self.dimension(),
            difficulty: state.difficulty,
            game_mode: self.game_mode(),
            hardcore: state.hardcore,
            max_players: state.max_players,
            server_brand: state.server_brand.clone(),
            min_y,
            height,
        }
    }

    fn eye_pos(&self) -> Option<Vec3> {
        self.client
            .try_query_self::<(&Position, &EntityDimensions), _>(|(p, d)| p.up(d.eye_height as f64))
            .ok()
    }

    fn eye_and_look(&self) -> Option<(Vec3, LookDirection)> {
        self.client
            .try_query_self::<(&Position, &EntityDimensions, &LookDirection), _>(|(p, d, l)| {
                (p.up(d.eye_height as f64), *l)
            })
            .ok()
    }
}

/// Upstream's `block2` position: populated only for a 'right' chest
/// half — the left-cardinal neighbour that completes a double chest
/// (`FACING_MAP`). 'single' chests carry no `block2` ('left' halves are
/// swallowed by the caller before this runs).
fn chest_other_half_pos(
    pos: BlockPos,
    type_prop: Option<&str>,
    facing: Option<&str>,
) -> Option<BlockPos> {
    if type_prop != Some("right") {
        return None;
    }
    let offset = match facing? {
        "north" => BlockPos { x: 1, y: 0, z: 0 },  // east
        "south" => BlockPos { x: -1, y: 0, z: 0 }, // west
        "west" => BlockPos { x: 0, y: 0, z: -1 },  // north
        "east" => BlockPos { x: 0, y: 0, z: 1 },   // south
        _ => return None,
    };
    Some(pos + offset)
}

/// Chest-lid dedup state (mineflayer's `openCountByPos`) — owned by the
/// bridge task, one per connection.
#[derive(Default)]
pub(crate) struct LidCounts(HashMap<BlockPos, u8>);

impl LidCounts {
    /// Port of the upstream `openCountByPos` check: emit when the stored
    /// count differs (an absent entry differs from everything).
    fn update(&mut self, pos: BlockPos, open_count: u8) -> bool {
        if self.0.get(&pos).copied() == Some(open_count) {
            return false;
        }
        if open_count > 0 {
            self.0.insert(pos, open_count);
        } else {
            self.0.remove(&pos);
        }
        true
    }
}

/// Feed one azalea [`Event`] into the world domain — the packet decode of
/// mineflayer's `plugins/blocks.js` + `time.js` + `rain.js` + `game.js`.
/// Azalea drops or internalizes these packets, so the bridge runs this
/// synchronously on every event (same loop position as the entity and
/// window observers): derived events keep packet order like upstream's
/// in-handler emits, and nothing rides a second broadcast hop that could
/// lag or drop.
///
/// `lids` is the bridge-kept `openCountByPos` dedup map — one per
/// connection, like upstream's.
pub(crate) fn observe_event(
    state: &RwLock<WorldState>,
    client: &azalea::Client,
    lids: &mut LidCounts,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        Event::Tick => {
            state.write().unwrap().tick_clocks();
            vec![]
        }
        Event::Packet(packet) => decode_packet(client, state, lids, packet),
        _ => vec![],
    }
}

fn decode_packet(
    client: &azalea::Client,
    state: &RwLock<WorldState>,
    lids: &mut LidCounts,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::BlockUpdate(p) => vec![block_update_event(
            client,
            &mut state.write().unwrap(),
            lids,
            p.pos,
            p.block_state,
        )],
        ClientboundGamePacket::SectionBlocksUpdate(p) => {
            let mut out = Vec::with_capacity(p.states.len());
            // one write guard across the batch — every entry updates the
            // tracker, so sequential updates to the same position see the
            // section's own earlier state as `old` (matching the packet
            // order the server applied them in).
            let mut state = state.write().unwrap();
            for s in &p.states {
                out.push(block_update_event(
                    client,
                    &mut state,
                    lids,
                    p.section_pos + s.pos,
                    s.state,
                ));
            }
            out
        }
        ClientboundGamePacket::ForgetLevelChunk(p) => {
            // A tracked position whose chunk unloads mustn't keep its
            // history — after a reload the world knows the real state,
            // and reporting the stale pre-unload value as `old` is worse
            // than the `old == new` fallback.
            state
                .write()
                .unwrap()
                .last_block_updates
                .retain(|pos, _| pos.x >> 4 != p.pos.x || pos.z >> 4 != p.pos.z);
            vec![BotEvent::ChunkUnload(p.pos)]
        }
        ClientboundGamePacket::SetTime(p) => {
            {
                let mut state = state.write().unwrap();
                state.game_time = p.game_time;
                client.with_registry_holder(|registries| {
                    for (clock, clock_state) in &p.clock_updates {
                        if let Some(key) = clock.key_owned(registries) {
                            state.clocks.insert(key.into_ident(), clock_state.clone());
                        }
                    }
                });
            }
            vec![BotEvent::Time]
        }
        ClientboundGamePacket::GameEvent(p) => {
            state.write().unwrap().apply_game_event(p.event, p.param)
        }
        ClientboundGamePacket::ChangeDifficulty(p) => {
            state.write().unwrap().difficulty = Some(p.difficulty);
            vec![]
        }
        ClientboundGamePacket::Login(p) => {
            {
                let mut state = state.write().unwrap();
                state.hardcore = Some(p.hardcore);
                state.max_players = Some(p.max_players);
                state.is_flat = Some(p.common.is_flat);
                // a new world means the tracked states describe stale
                // positions — drop them rather than report them as `old`
                state.last_block_updates.clear();
            }
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::Respawn(p) => {
            let mut state = state.write().unwrap();
            state.is_flat = Some(p.common.is_flat);
            state.last_block_updates.clear();
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::SetDefaultSpawnPosition(p) => {
            state.write().unwrap().spawn_point = Some(p.global_pos.clone());
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::CustomPayload(p) => {
            // `minecraft:brand` → bot.game.serverBrand.
            if p.identifier.to_string() == "minecraft:brand"
                && let Ok(brand) = String::azalea_read(&mut Cursor::new(&p.data.0[..]))
            {
                state.write().unwrap().server_brand = Some(brand);
            }
            vec![]
        }
        ClientboundGamePacket::OpenSignEditor(p) => vec![BotEvent::SignOpen {
            block: block_at(client, p.pos),
            is_front_text: p.is_front_text,
        }],
        ClientboundGamePacket::BlockEntityData(p) => {
            vec![BotEvent::BlockEntityData(p.clone())]
        }
        ClientboundGamePacket::BlockDestruction(p) => {
            let entity = crate::entity::entity_by_minecraft_id(client, p.id);
            let block = block_at(client, p.pos);
            if p.progress <= 9 {
                vec![BotEvent::BlockBreakProgress {
                    block,
                    stage: p.progress,
                    entity,
                }]
            } else {
                vec![BotEvent::BlockBreakEnd { block, entity }]
            }
        }
        ClientboundGamePacket::BlockEvent(p) => block_event(client, lids, p),
        _ => vec![],
    }
}

fn block_update_event(
    client: &azalea::Client,
    state: &mut WorldState,
    lids: &mut LidCounts,
    pos: BlockPos,
    new_state: BlockState,
) -> BotEvent {
    // azalea's `handle_block_update_event` drains the queued updates in
    // the same `Update` pass that emits `Event::Packet`, and the async
    // bridge can't observe mid-pass (`run_schedule_loop` only yields
    // between iterations), so the live world already reports `new_state`
    // here. `oldBlock` therefore comes from our own update history;
    // positions with no history fall back to the post-apply read —
    // `old == new` for a first update, the best available without
    // shadowing every loaded block.
    let old = state
        .update_and_old(pos, new_state)
        .or_else(|| with_world(client, |w| w.get_block_state(pos)).flatten());
    if old.map(|s| s.as_block_kind()) != Some(new_state.as_block_kind()) {
        // upstream drops the lid count when the block type changes.
        lids.0.remove(&pos);
    }
    BotEvent::BlockUpdate {
        old: old.map(|state| Block::new(client, pos, state)),
        new: Block::new(client, pos, new_state),
    }
}

fn block_event(
    client: &azalea::Client,
    lids: &mut LidCounts,
    p: &ClientboundBlockEvent,
) -> Vec<BotEvent> {
    let Some(block) = block_at(client, p.pos) else {
        return vec![];
    };
    match p.block {
        BlockKind::Piston | BlockKind::StickyPiston => vec![BotEvent::PistonMove {
            block,
            is_pulling: p.action_id != 0,
            direction: p.action_parameter,
        }],
        BlockKind::NoteBlock => vec![], // 'noteHeard' belongs to the misc domain.
        _ => {
            // 'left' halves of a double chest are swallowed upstream.
            let second = match p.block {
                BlockKind::Chest | BlockKind::TrappedChest => {
                    if block.property("type") == Some("left") {
                        return vec![];
                    }
                    chest_other_half_pos(p.pos, block.property("type"), block.property("facing"))
                        .and_then(|pos| block_at(client, pos))
                }
                _ => None,
            };
            // 'chestLidMove' in upstream fires for any other block event —
            // chests and other lid-animated containers alike.
            if !lids.update(p.pos, p.action_parameter) {
                return vec![];
            }
            vec![BotEvent::ChestLidMove {
                block,
                open_count: p.action_parameter,
                second,
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use azalea::{
        block::BlockState,
        core::entity_id::MinecraftEntityId,
        protocol::packets::game::c_block_destruction::ClientboundBlockDestruction,
        registry::builtin::BlockKind,
        world::{Chunk, PartialChunkStorage},
    };

    use super::*;

    fn stone() -> BlockState {
        BlockState::from(BlockKind::Stone)
    }

    /// A 3×3-chunk `World` for tests. Returns the `PartialChunkStorage`
    /// too — `ChunkStorage` only holds `Weak` chunk references, so the
    /// caller must keep the partial alive or `get` returns `None`.
    fn test_world() -> (World, PartialChunkStorage) {
        let mut world = World::default();
        let mut partial = PartialChunkStorage::default();
        for x in -1..=1 {
            for z in -1..=1 {
                partial.set(
                    &ChunkPos { x, z },
                    Some(Chunk::default()),
                    &mut world.chunks,
                );
            }
        }
        (world, partial)
    }

    #[test]
    fn find_blocks_closest_first_and_count() {
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 8, y: 0, z: 0 }, stone());
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 5, z: 0 }, stone());
        let states = BlockStates::from([BlockKind::Stone]);

        let found = find_states(&world, BlockPos::default(), &states, 16., 10);
        assert_eq!(
            found,
            vec![
                BlockPos { x: 0, y: 0, z: 3 },
                BlockPos { x: 0, y: 5, z: 0 },
                BlockPos { x: 8, y: 0, z: 0 },
            ]
        );
        let found = find_states(&world, BlockPos::default(), &states, 16., 2);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn find_blocks_respects_max_distance() {
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 10, y: 0, z: 0 }, stone());
        let states = BlockStates::from([BlockKind::Stone]);
        assert!(find_states(&world, BlockPos::default(), &states, 4., 1).is_empty());
        assert_eq!(
            find_states(&world, BlockPos::default(), &states, 16., 1),
            vec![BlockPos { x: 10, y: 0, z: 0 }]
        );
    }

    #[test]
    fn find_blocks_predicate() {
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 2, y: 0, z: 0 }, stone());
        world
            .chunks
            .set_block_state(BlockPos { x: -2, y: 0, z: 0 }, stone());
        let found = find_matching(
            &world,
            BlockPos::default(),
            &|pos, state| pos.x > 0 && state == stone(),
            16.,
            10,
        );
        assert_eq!(found, vec![BlockPos { x: 2, y: 0, z: 0 }]);
    }

    #[test]
    fn raycast_hits_and_misses() {
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
        let from = Vec3 {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        };
        assert_eq!(
            raycast_blocks(
                &world,
                from,
                Vec3 {
                    x: 0.,
                    y: 0.,
                    z: 1.
                },
                10.,
                None
            )
            .map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 0, z: 3 })
        );
        // a rejecting matcher must pass the block by.
        assert!(
            raycast_blocks(
                &world,
                from,
                Vec3 {
                    x: 0.,
                    y: 0.,
                    z: 1.
                },
                10.,
                Some(&|_, _| false)
            )
            .is_none()
        );
        // nothing but air along +x.
        assert!(
            raycast_blocks(
                &world,
                from,
                Vec3 {
                    x: 1.,
                    y: 0.,
                    z: 0.
                },
                10.,
                None
            )
            .is_none()
        );
    }

    fn short_grass() -> BlockState {
        BlockState::from(BlockKind::ShortGrass)
    }

    #[test]
    fn raycast_no_matcher_clips_collision_shapes() {
        // Short grass has an outline but *no* collision shape — the
        // no-matcher ray (upstream `iter.intersect(block.shapes)`)
        // passes through it to the stone behind.
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 0, z: 2 }, short_grass());
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 0, z: 3 }, stone());
        let from = Vec3 {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        };
        let dir = Vec3 {
            x: 0.,
            y: 0.,
            z: 1.,
        };
        assert_eq!(
            raycast_blocks(&world, from, dir, 10., None).map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 0, z: 3 })
        );
    }

    #[test]
    fn raycast_matcher_accepts_voxel_entry() {
        // Upstream `WorldSync.raycast`: with a matcher the first voxel
        // it accepts wins — no shape clip — so matchers reach
        // empty/partial shapes and intervening blocks never clip.
        let (world, _partial) = test_world();
        let target = BlockPos { x: 0, y: 0, z: 3 };
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 0, z: 2 }, stone());
        world.chunks.set_block_state(target, short_grass());
        let from = Vec3 {
            x: 0.5,
            y: 0.5,
            z: 0.5,
        };
        let dir = Vec3 {
            x: 0.,
            y: 0.,
            z: 1.,
        };
        // no matcher: the stone's collision shape wins.
        assert_eq!(
            raycast_blocks(&world, from, dir, 10., None).map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 0, z: 2 })
        );
        // a matcher sails past the stone and accepts the grass on entry.
        assert_eq!(
            raycast_blocks(&world, from, dir, 10., Some(&|pos, _| pos == target))
                .map(|(pos, _)| pos),
            Some(target)
        );
    }

    #[test]
    fn can_see_block_partial_outline() {
        // The eye→voxel-centre ray never clips an empty-collision block
        // (short grass), yet `can_see_accepts` accepts it on voxel
        // entry — the case the outline-clip version false-negatived on.
        let (world, _partial) = test_world();
        let target = BlockPos { x: 0, y: 0, z: 3 };
        world.chunks.set_block_state(target, short_grass());
        let from = Vec3 {
            x: 0.5,
            y: 2.5,
            z: 0.5,
        };
        let to = Vec3 {
            x: 0.5,
            y: 0.5,
            z: 3.5,
        };
        let range = from.distance_to(to);
        let dir = (to - from) * (1. / range);
        let matcher = |pos, state| can_see_accepts(target, from, to, pos, state);
        // nothing clips (grass has no collision shape), but the target's
        // voxel is entered → accepted.
        assert!(raycast_blocks(&world, from, dir, range, None).is_none());
        assert_eq!(
            raycast_blocks(&world, from, dir, range, Some(&matcher)).map(|(pos, _)| pos),
            Some(target)
        );
        // an intervening block whose collision clips is accepted first
        // → the target is *not* seen through it.
        world
            .chunks
            .set_block_state(BlockPos { x: 0, y: 1, z: 2 }, stone());
        assert_eq!(
            raycast_blocks(&world, from, dir, range, Some(&matcher)).map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 1, z: 2 })
        );
    }

    #[test]
    fn chest_other_half_only_for_right() {
        let pos = BlockPos { x: 0, y: 64, z: 0 };
        // 'single' chests carry no `block2`.
        assert_eq!(
            chest_other_half_pos(pos, Some("single"), Some("north")),
            None
        );
        // 'left' halves carry none either (the event is swallowed).
        assert_eq!(chest_other_half_pos(pos, Some("left"), Some("north")), None);
        // 'right' halves point at the left-cardinal neighbour —
        // north-facing → east (upstream FACING_MAP).
        assert_eq!(
            chest_other_half_pos(pos, Some("right"), Some("north")),
            Some(BlockPos { x: 1, y: 64, z: 0 })
        );
        // missing properties → no second.
        assert_eq!(chest_other_half_pos(pos, None, Some("north")), None);
        assert_eq!(chest_other_half_pos(pos, Some("right"), None), None);
    }

    #[test]
    fn sign_lines_validates() {
        assert!(sign_lines("a\nb").is_ok());
        assert_eq!(sign_lines("a\nb").unwrap()[2], "");
        assert!(sign_lines("a\nb\nc\nd").is_ok());
        assert!(sign_lines("a\nb\nc\nd\ne").is_err());
        assert!(sign_lines(&"x".repeat(46)).is_err());
    }

    #[test]
    fn game_events_drive_weather() {
        let mut state = WorldState::default();
        assert!(matches!(
            state
                .apply_game_event(EventType::StartRaining, 0.)
                .as_slice(),
            [BotEvent::Rain]
        ));
        assert!(state.is_raining);
        // a rising level while already raining: WeatherUpdate only.
        assert!(matches!(
            state
                .apply_game_event(EventType::RainLevelChange, 0.5)
                .as_slice(),
            [BotEvent::WeatherUpdate]
        ));
        assert_eq!(state.rain_state, 0.5);
        // level dropping to 0 while raining flips is_raining off.
        assert!(matches!(
            state
                .apply_game_event(EventType::RainLevelChange, 0.)
                .as_slice(),
            [BotEvent::WeatherUpdate, BotEvent::Rain]
        ));
        assert!(!state.is_raining);
        assert!(matches!(
            state
                .apply_game_event(EventType::ThunderLevelChange, 0.3)
                .as_slice(),
            [BotEvent::WeatherUpdate]
        ));
        assert!(matches!(
            state
                .apply_game_event(EventType::ChangeGameMode, 0.)
                .as_slice(),
            [BotEvent::GameStateChanged]
        ));
    }

    #[test]
    fn clocks_tick_and_compute_time() {
        let mut state = WorldState {
            game_time: 999,
            ..Default::default()
        };
        state.clocks.insert(
            Identifier::from("minecraft:overworld"),
            ClockState {
                total_ticks: 25001,
                partial_tick: 0.6,
                rate: 1.,
            },
        );
        state.tick_clocks();
        let clock = &state.clocks[&Identifier::from("minecraft:overworld")];
        assert_eq!(clock.total_ticks, 25002);
        assert!((clock.partial_tick - 0.6).abs() < f32::EPSILON);

        let time = Time::compute(
            &state,
            Some(&WorldName(Identifier::from("minecraft:overworld"))),
        )
        .unwrap();
        assert_eq!(time.time, 25002);
        assert_eq!(time.time_of_day, 1002);
        assert_eq!(time.day, 1);
        assert!(time.is_day);
        assert_eq!(time.moon_phase, 1);
        assert_eq!(time.age, 999);
        assert!(time.do_daylight_cycle);

        // nether falls back to the overworld clock.
        let nether = Time::compute(
            &state,
            Some(&WorldName(Identifier::from("minecraft:the_nether"))),
        )
        .unwrap();
        assert_eq!(nether.time, 25002);
        assert_eq!(nether.time_of_day, 1002);
    }

    #[test]
    fn lid_counts_dedup() {
        let mut lids = LidCounts::default();
        let pos = BlockPos { x: 1, y: 2, z: 3 };
        assert!(lids.update(pos, 1));
        assert!(!lids.update(pos, 1));
        assert!(lids.update(pos, 0));
        assert!(lids.update(pos, 2));
    }

    #[test]
    fn block_destruction_after_self_despawn_still_emits() {
        let client = crate::test_util::despawned_client();
        let state = RwLock::new(WorldState::default());
        let mut lids = LidCounts::default();
        // the breaker id resolves through our own entity's index — once it
        // is despawned the lookup must miss, not panic.
        let out = decode_packet(
            &client,
            &state,
            &mut lids,
            &ClientboundGamePacket::BlockDestruction(ClientboundBlockDestruction {
                id: MinecraftEntityId(1),
                pos: BlockPos::new(0, 60, 0),
                progress: 4,
            }),
        );
        assert!(matches!(
            &out[..],
            [BotEvent::BlockBreakProgress {
                stage: 4,
                entity: None,
                ..
            }]
        ));
    }

    #[test]
    fn observe_event_decodes_packets_in_call_order() {
        use azalea::protocol::packets::game::{
            c_forget_level_chunk::ClientboundForgetLevelChunk, c_game_event::ClientboundGameEvent,
        };

        let client = crate::test_util::despawned_client();
        let state = RwLock::new(WorldState::default());
        let mut lids = LidCounts::default();
        let packet = |p| Event::Packet(Arc::new(p));

        // The decode used to ride a second broadcast hop: events could land
        // out of packet order and `Lagged` silently dropped them. Running
        // inside the bridge's event loop makes the emission synchronous —
        // this sequence is asserted in the order the packets arrived.
        // Seed one tracked update inside the unloaded chunk and one
        // outside: the unload drops only the inside entry.
        {
            let mut s = state.write().unwrap();
            s.update_and_old(BlockPos::new(48 + 5, 64, 64 + 2), stone()); // chunk (3,4)
            s.update_and_old(BlockPos::new(0, 64, 0), stone()); // chunk (0,0)
        }
        let out = observe_event(
            &state,
            &client,
            &mut lids,
            &packet(ClientboundGamePacket::ForgetLevelChunk(
                ClientboundForgetLevelChunk {
                    pos: ChunkPos::new(3, 4),
                },
            )),
        );
        assert!(matches!(&out[..], [BotEvent::ChunkUnload(_)]));
        {
            let s = state.read().unwrap();
            assert!(
                !s.last_block_updates
                    .contains_key(&BlockPos::new(53, 64, 66))
            );
            assert!(s.last_block_updates.contains_key(&BlockPos::new(0, 64, 0)));
        }
        let out = observe_event(
            &state,
            &client,
            &mut lids,
            &packet(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
                event: EventType::StartRaining,
                param: 0.,
            })),
        );
        assert!(matches!(&out[..], [BotEvent::Rain]));
        // state is updated by the same call, not a later task hop
        assert!(state.read().unwrap().is_raining);
    }

    #[test]
    fn observe_event_ticks_clocks_on_tick() {
        let client = crate::test_util::despawned_client();
        let state = RwLock::new(WorldState {
            clocks: HashMap::from([(
                Identifier::from("minecraft:overworld"),
                ClockState {
                    total_ticks: 100,
                    partial_tick: 0.9,
                    rate: 1.,
                },
            )]),
            ..Default::default()
        });
        let mut lids = LidCounts::default();
        assert!(observe_event(&state, &client, &mut lids, &Event::Tick).is_empty());
        let clock = &state.read().unwrap().clocks[&Identifier::from("minecraft:overworld")];
        assert_eq!(clock.total_ticks, 101);
        assert!((clock.partial_tick - 0.9).abs() < f32::EPSILON);
        // non-packet events decode to nothing
        assert!(observe_event(&state, &client, &mut lids, &Event::Login).is_empty());
    }

    /// C2: `handle_block_update_event` applies the update inside the
    /// same `Update` pass that emits `Event::Packet`, and the bridge
    /// can't observe mid-pass — so a live world read can't supply
    /// `oldBlock`. The tracked update history must: a re-updated
    /// position reports its last-reported state even though the world
    /// already shows the new one.
    #[test]
    fn block_update_old_comes_from_update_history() {
        use azalea::protocol::packets::game::c_block_update::ClientboundBlockUpdate;

        let client = crate::test_util::live_entity_client(());
        let (world, _partial) = test_world();
        let shared = std::sync::Arc::new(parking_lot::RwLock::new(world));
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(WorldHolder::new(client.entity, shared.clone()));

        let state = RwLock::new(WorldState::default());
        let mut lids = LidCounts::default();
        let pos = BlockPos::new(1, 64, 1);
        let dirt = BlockState::from(BlockKind::Dirt);
        let packet = |block_state| {
            Event::Packet(Arc::new(ClientboundGamePacket::BlockUpdate(
                ClientboundBlockUpdate { pos, block_state },
            )))
        };

        // Simulate azalea's real ordering: `handle_block_update_event`
        // already wrote the new state into the world when this packet's
        // `Event::Packet` is decoded.
        shared.write().chunks.set_block_state(pos, stone());
        let out = observe_event(&state, &client, &mut lids, &packet(stone()));
        // A never-before-updated position has no history — the only
        // answer available is the post-apply read, which echoes the new
        // state back as `old` (the documented limit).
        assert!(matches!(
            &out[..],
            [BotEvent::BlockUpdate {
                old: Some(old),
                new
            }] if old.state == stone() && new.state == stone()
        ));

        // Second update: history wins over the post-apply read — `old`
        // is stone even though the world already shows dirt.
        shared.write().chunks.set_block_state(pos, dirt);
        let out = observe_event(&state, &client, &mut lids, &packet(dirt));
        assert!(matches!(
            &out[..],
            [BotEvent::BlockUpdate {
                old: Some(old),
                new
            }] if old.state == stone() && new.state == dirt
        ));
    }

    /// The tracking bound clears wholesale at the cap — the next update
    /// reports its post-apply read again instead of a stale state.
    #[test]
    fn update_history_is_bounded() {
        let mut state = WorldState::default();
        for x in 0..MAX_TRACKED_UPDATES as i32 {
            assert!(
                state
                    .update_and_old(BlockPos::new(x, 0, 0), stone())
                    .is_none()
            );
        }
        // at capacity: re-updating a *tracked* position still returns
        // its predecessor — hot positions aren't flushed wholesale
        let pos = BlockPos::new(5, 0, 0);
        assert_eq!(
            state.update_and_old(pos, BlockState::from(BlockKind::Dirt)),
            Some(stone())
        );
        // a *new* position at capacity clears wholesale and records
        // fresh — it reports `None` (no history) again
        let fresh = BlockPos::new(MAX_TRACKED_UPDATES as i32, 0, 0);
        assert!(state.update_and_old(fresh, stone()).is_none());
        assert!(state.last_block_updates.len() <= MAX_TRACKED_UPDATES);
    }

    /// `findBlocks`' `||` coercion: `maxDistance: 0`/`count: 0` are falsy
    /// upstream and take the default just like an unset option.
    #[test]
    fn find_blocks_falsy_defaults() {
        let client = crate::test_util::live_entity_client(());
        let (world, _partial) = test_world();
        world
            .chunks
            .set_block_state(BlockPos { x: 10, y: 0, z: 0 }, stone());
        let shared = std::sync::Arc::new(parking_lot::RwLock::new(world));
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(WorldHolder::new(client.entity, shared));
        let world = WorldRef::new(client);

        // `max_distance: 0` upstream falls back to 16 — far enough to
        // see the stone 10 blocks out; `count: 0` falls back to 1.
        let mut opts = FindBlocksOpts::new(BlockKind::Stone);
        opts.max_distance = Some(0.);
        opts.count = Some(0);
        assert_eq!(
            world.find_blocks(&opts),
            vec![BlockPos { x: 10, y: 0, z: 0 }]
        );
        // NaN is falsy too; a nonzero override still wins.
        opts.max_distance = Some(f64::NAN);
        opts.count = Some(5);
        assert_eq!(world.find_blocks(&opts).len(), 1);
        opts.max_distance = Some(4.);
        assert!(world.find_blocks(&opts).is_empty());
    }

    #[test]
    fn location_fields_match_upstream_formulas() {
        let loc = Location::new(Vec3 {
            x: 20.5,
            y: 70.0,
            z: -18.2,
        });
        assert_eq!(loc.floored, BlockPos::new(20, 70, -19));
        // upstream `modulus` floors the remainder: -19 -> 13, not -3
        assert_eq!(loc.block_point, BlockPos::new(4, 6, 13));
        assert_eq!(loc.chunk_corner, BlockPos::new(16, 64, -32));
        assert_eq!(loc.block_index, 4 + 16 * 13 + 256 * 6);
        assert_eq!(loc.biome_block_index, 4 + 16 * 13);
        // chunkYIndex floors the *unfloored* absolute y / 16
        assert_eq!(loc.chunk_y_index, 4);
    }

    #[test]
    fn location_negative_y_wraps_like_upstream() {
        let loc = Location::new(Vec3 {
            x: 0.,
            y: -0.5,
            z: 0.,
        });
        assert_eq!(loc.floored, BlockPos::new(0, -1, 0));
        assert_eq!(loc.block_point, BlockPos::new(0, 15, 0));
        assert_eq!(loc.chunk_corner, BlockPos::new(0, -16, 0));
        assert_eq!(loc.block_index, 256 * 15);
        assert_eq!(loc.chunk_y_index, -1);
    }
}
