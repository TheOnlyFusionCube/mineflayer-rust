//! world — `prismarine-world`/`prismarine-block` analog over the engine's world
//! storage, plus the facade-kept world/game state (time, weather, spawn
//! point, difficulty) that the engine does not track and which is therefore
//! recovered here from raw `ClientboundGamePacket`s.

use std::{collections::HashMap, io::Cursor};

use parking_lot::RwLock;
use tracing::warn;

use mineflayer_engine::{
    Event,
    block::{BlockState, BlockStates, BlockTrait},
    buf::McBuf,
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

mod tracker;

pub(crate) use tracker::{LidCounts, WorldState, observe_event};

const DEFAULT_MAX_DISTANCE: f64 = 16.;
const DEFAULT_COUNT: usize = 1;
/// mineflayer's `blockAtCursor` default max distance (256 blocks).
pub const DEFAULT_CURSOR_DISTANCE: f64 = 256.;
/// mineflayer's `entityAtCursor` default max distance (3.5 blocks).
pub const DEFAULT_ENTITY_CURSOR_DISTANCE: f64 = 3.5;

/// The minecraft world the bot is in — the `prismarine-world` analog.
///
/// Wraps the engine [`Client`](mineflayer_engine::Client), which owns the shared
/// `mineflayer_engine::world::World`; reach the raw world handle through
/// [`WorldRef::client`] (`client.world()` / `client.partial_world()`).
/// `None`-returning methods simply indicate the chunk (or world) isn't
/// loaded.
#[derive(Clone)]
pub struct WorldRef {
    client: mineflayer_engine::Client,
}

impl std::fmt::Debug for WorldRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorldRef")
    }
}

impl WorldRef {
    pub(crate) fn new(client: mineflayer_engine::Client) -> Self {
        Self { client }
    }

    /// The engine client that owns the underlying `mineflayer_engine::world::World`.
    pub fn client(&self) -> &mineflayer_engine::Client {
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
    ///
    /// A matcher runs with no world lock held (see
    /// [`BlockMatcher::Predicate`]): the traversal collects its cells
    /// under one lock, then the matcher sees them after the release.
    pub fn raycast(
        &self,
        from: Vec3,
        direction: Vec3,
        range: f64,
        matcher: Option<&BlockMatcher>,
    ) -> Option<Block> {
        match matcher {
            None => with_world(&self.client, |w| raycast_blocks(w, from, direction, range))
                .flatten()
                .map(|(pos, state)| Block::new(&self.client, pos, state)),
            Some(m) => raycast_unlocked(&self.client, from, direction, range, m),
        }
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
        match &opts.matching {
            BlockMatcher::States(states) => with_world(&self.client, |w| {
                find_states(w, point, states, max_distance, count)
            })
            .unwrap_or_default(),
            // the predicate runs outside the world lock — see
            // `find_matching_unlocked`
            BlockMatcher::Predicate(pred) => {
                find_matching_unlocked(&self.client, point, pred.as_ref(), max_distance, count)
            }
        }
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
    pub(crate) fn new(
        client: &mineflayer_engine::Client,
        pos: BlockPos,
        state: BlockState,
    ) -> Self {
        Self {
            pos,
            state,
            world: WorldRef::new(client.clone()),
        }
    }

    /// `block.name` — e.g. `"minecraft:grass_block"`. Diverges from
    /// upstream, whose `block.name` strips the namespace
    /// (`"grass_block"`); [`Block::short_name`] is upstream's form, like
    /// [`crate::Item::name`].
    pub fn name(&self) -> &'static str {
        self.state.as_block_kind().to_str()
    }

    /// Upstream's `block.name` — the name without the `minecraft:`
    /// namespace (`"grass_block"`).
    pub fn short_name(&self) -> &'static str {
        let name = self.name();
        name.strip_prefix("minecraft:").unwrap_or(name)
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

    /// `block.boundingBox` — [`BoundingBox::Block`] when the outline shape
    /// is non-empty. [`BoundingBox::as_str`] gives upstream's string.
    pub fn bounding_box(&self) -> BoundingBox {
        if self.state.outline_shape().is_empty() {
            BoundingBox::Empty
        } else {
            BoundingBox::Block
        }
    }

    /// `block.diggable` — the prismarine `diggable` flag: false for
    /// bedrock-like blocks (`hardness < 0`) and the air kinds (hardness 0
    /// but flagged undiggable in the data); fluids stay diggable like
    /// upstream.
    pub fn diggable(&self) -> bool {
        !matches!(
            self.kind(),
            BlockKind::Air | BlockKind::VoidAir | BlockKind::CaveAir
        ) && self.behavior().destroy_time >= 0.
    }

    /// `block.material`/`block.harvestTools` backing data — the engine's
    /// `BlockBehavior` (friction, destroy time, …). U6 uses `destroy_time`,
    /// `requires_correct_tool_for_drops` and friends for dig timing.
    pub fn behavior(&self) -> mineflayer_engine::block::BlockBehavior {
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

/// `block.boundingBox` — upstream's `'block'`/`'empty'` string as an enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundingBox {
    /// The block has a non-empty outline shape (`'block'`).
    Block,
    /// The block has no outline shape, like air (`'empty'`).
    Empty,
}

impl BoundingBox {
    /// Upstream's string: `"block"` or `"empty"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Empty => "empty",
        }
    }
}

/// `matching` argument for [`Bot::find_blocks`]/[`Bot::block_at_cursor`] —
/// either a set of block kinds/states or an arbitrary predicate.
pub enum BlockMatcher {
    /// Fast path: match any of these block states (palette-filtered).
    States(BlockStates),
    /// Arbitrary predicate over the resolved [`Block`].
    ///
    /// The predicate runs without a world lock. It may read the world
    /// again (for example another `block_at`) without risking a deadlock.
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
/// lock type.
fn with_world<R>(client: &mineflayer_engine::Client, f: impl FnOnce(&World) -> R) -> Option<R> {
    let Some(holder) = client.get_component::<WorldHolder>() else {
        warn!("the client's WorldHolder is gone (torn-down client) — block reads return None");
        return None;
    };
    let world = holder.shared.read();
    Some(f(&world))
}

fn block_at(client: &mineflayer_engine::Client, pos: BlockPos) -> Option<Block> {
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

/// One chunk's in-range `(pos, state)` cells, in scan order — the part of
/// a predicate search that needs the world lock. The caller runs the user
/// predicate after it releases the lock (see [`find_matching_unlocked`]).
fn collect_chunk_cells(
    world: &World,
    chunk_pos: ChunkPos,
    point: BlockPos,
    max_distance: f64,
) -> Vec<(BlockPos, BlockState)> {
    let min_y = world.chunks.min_y();
    let mut cells = vec![];
    if let Some(chunk) = world.chunks.get(&chunk_pos) {
        let chunk = chunk.read();
        for (i, section) in chunk.sections.iter().enumerate() {
            let base_y = min_y + i as i32 * 16;
            for idx in 0..4096usize {
                let sp = section.states.pos_from_index(idx);
                let pos = BlockPos {
                    x: chunk_pos.x * 16 + sp.x as i32,
                    y: base_y + sp.y as i32,
                    z: chunk_pos.z * 16 + sp.z as i32,
                };
                if euclidean(pos, point) <= max_distance {
                    cells.push((pos, section.states.get_at_index(idx)));
                }
            }
        }
    }
    cells
}

/// Predicate-mode find — scans every loaded section index (no palette
/// fast-path is possible for arbitrary predicates).
///
/// The user predicate runs with no world lock held: each chunk's in-range
/// cells are collected under a short lock (`collect_chunk_cells`), the
/// lock is released, and only then does the predicate see them. A
/// `parking_lot` read lock is not re-entrant when a writer waits, so a
/// predicate that reads the world (for example another `block_at`) would
/// otherwise deadlock. Results match the previous in-lock scan exactly:
/// same scan order, same closest-first sort, same count truncation.
fn find_matching_unlocked(
    client: &mineflayer_engine::Client,
    point: BlockPos,
    matches: &dyn Fn(&Block) -> bool,
    max_distance: f64,
    count: usize,
) -> Vec<BlockPos> {
    let mut found = vec![];
    let layers = ((max_distance + 8.) / 16.).ceil().max(1.) as u32;
    let mut iter = ChunkIterator::new(ChunkPos::from(point), layers);
    let mut started_layer = 0;
    while let Some(chunk_pos) = iter.next() {
        let cells = with_world(client, |w| {
            collect_chunk_cells(w, chunk_pos, point, max_distance)
        })
        .unwrap_or_default();
        for (pos, state) in cells {
            if matches(&Block::new(client, pos, state)) {
                found.push(pos);
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

/// Matcher-mode `world.raycast` with the matcher outside the world lock:
/// the traversal collects its cells under one lock (bounded by the ray's
/// length — the traversal never terminates early here), then the matcher
/// visits them in traversal order. Matchers win on voxel entry (empty or
/// partial shapes the ray only grazes are reachable), faithful to upstream
/// `WorldSync.raycast`.
fn raycast_unlocked(
    client: &mineflayer_engine::Client,
    from: Vec3,
    direction: Vec3,
    range: f64,
    matcher: &BlockMatcher,
) -> Option<Block> {
    let len = direction.length();
    if range <= 0. || len == 0. {
        return None;
    }
    let to = from + direction * (range / len);
    let cells = with_world(client, |w| {
        // `traverse_blocks` takes an `Fn`, not `FnMut`; the visitor runs on
        // this thread only
        let cells = std::cell::RefCell::new(vec![]);
        let _ = traverse_blocks(
            from,
            to,
            (),
            |_, pos| -> Option<Option<(BlockPos, BlockState)>> {
                if let Some(state) = w.chunks.get_block_state(pos) {
                    cells.borrow_mut().push((pos, state));
                }
                None
            },
            |_| None,
        );
        cells.into_inner()
    })?;
    cells.into_iter().find_map(|(pos, state)| {
        let block = Block::new(client, pos, state);
        matcher.matches(&block).then_some(block)
    })
}

/// `world.raycast` core — Amanatides & Woo DDA via the engine's
/// `traverse_blocks`, faithful to upstream `WorldSync.raycast`: the first
/// block whose *collision* shape is clipped wins (upstream's
/// `iter.intersect(block.shapes)`). Matcher-mode rays go through
/// [`raycast_unlocked`] so the user matcher stays outside the world lock.
fn raycast_blocks(
    world: &World,
    from: Vec3,
    direction: Vec3,
    range: f64,
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
            state
                .collision_shape()
                .clip(from, to, pos)
                .map(|_| Some((pos, state)))
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
            mineflayer_engine::entity::view_vector(look),
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

    /// `bot.entityAtCursor(maxDistance)` — divergence: the engine only exposes
    /// the crosshair hit its own `pick` plugin computes (interaction +
    /// block ranges, not an arbitrary distance), so `max_distance` acts as
    /// a cap on that result rather than driving an independent raycast.
    /// Default upstream: 3.5.
    pub fn entity_at_cursor(&self, max_distance: f64) -> Option<mineflayer_engine::EntityRef> {
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
        entity: &mineflayer_engine::EntityRef,
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
            mineflayer_engine::entity::view_vector(look),
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
    /// Reading sign text is a known divergence: the engine doesn't store block
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
    /// (the engine drops it). `None` until the packet arrives (upstream reads
    /// an unset spawn point as `undefined`, not the origin).
    pub fn spawn_point(&self) -> Option<BlockPos> {
        self.shared
            .world_state
            .read()
            .spawn_point
            .as_ref()
            .map(|g| g.pos)
    }

    /// `bot.time` — `None` until the first `set_time` packet.
    pub fn time(&self) -> Option<Time> {
        let state = self.shared.world_state.read();
        Time::compute(&state, self.dimension().as_ref())
    }

    /// `bot.isRaining`.
    pub fn is_raining(&self) -> bool {
        self.shared.world_state.read().is_raining
    }

    /// `bot.rainState` — 0.0..1.0.
    pub fn rain_state(&self) -> f32 {
        self.shared.world_state.read().rain_state
    }

    /// `bot.thunderState` — 0.0..1.0.
    pub fn thunder_state(&self) -> f32 {
        self.shared.world_state.read().thunder_state
    }

    /// `bot.game` — snapshot; `None` fields weren't seen yet.
    pub fn game(&self) -> Game {
        let (min_y, height) = with_world(&self.client, |w| (w.chunks.min_y(), w.chunks.height()))
            .map_or((None, None), |(y, h)| (Some(y), Some(h)));
        let state = self.shared.world_state.read();
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

#[cfg(test)]
mod tests {
    use mineflayer_engine::{
        block::BlockState,
        registry::builtin::BlockKind,
        world::{Chunk, PartialChunkStorage},
    };

    use super::*;

    pub(super) fn stone() -> BlockState {
        BlockState::from(BlockKind::Stone)
    }

    /// prismarine's `diggable` flag: the air kinds are undiggable despite
    /// hardness 0; bedrock-like blocks are undiggable; fluids stay
    /// diggable like upstream's data.
    #[test]
    fn diggable_matches_the_prismarine_flag() {
        let client = crate::test_util::despawned_client();
        let block = |kind| Block::new(&client, BlockPos::new(0, 64, 0), BlockState::from(kind));
        for kind in [BlockKind::Air, BlockKind::VoidAir, BlockKind::CaveAir] {
            assert!(!block(kind).diggable(), "{kind:?} must not be diggable");
        }
        for kind in [BlockKind::Stone, BlockKind::Water, BlockKind::ShortGrass] {
            assert!(block(kind).diggable(), "{kind:?} must be diggable");
        }
    }

    /// `boundingBox` is typed but keeps upstream's strings; `short_name`
    /// is upstream's un-namespaced `block.name` (B5).
    #[test]
    fn bounding_box_and_short_name_match_upstream() {
        let client = crate::test_util::despawned_client();
        let block = |kind| Block::new(&client, BlockPos::new(0, 64, 0), BlockState::from(kind));
        assert_eq!(block(BlockKind::Stone).bounding_box(), BoundingBox::Block);
        assert_eq!(block(BlockKind::Air).bounding_box(), BoundingBox::Empty);
        assert_eq!(BoundingBox::Block.as_str(), "block");
        assert_eq!(BoundingBox::Empty.as_str(), "empty");
        assert_eq!(block(BlockKind::GrassBlock).name(), "minecraft:grass_block");
        assert_eq!(block(BlockKind::GrassBlock).short_name(), "grass_block");
    }

    /// A 3×3-chunk `World` for tests. Returns the `PartialChunkStorage`
    /// too — `ChunkStorage` only holds `Weak` chunk references, so the
    /// caller must keep the partial alive or `get` returns `None`.
    pub(super) fn test_world() -> (World, PartialChunkStorage) {
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

    /// A `WorldRef` over a fresh 3×3 test world with `blocks` placed,
    /// plus the shared world (for mutations between assertions) and the
    /// partial storage the caller must keep alive.
    pub(super) fn world_view(
        blocks: &[(BlockPos, BlockState)],
    ) -> (
        WorldRef,
        std::sync::Arc<parking_lot::RwLock<World>>,
        PartialChunkStorage,
    ) {
        let client = crate::test_util::live_entity_client(());
        let (world, partial) = test_world();
        for (pos, state) in blocks {
            world.chunks.set_block_state(*pos, *state);
        }
        let shared = std::sync::Arc::new(parking_lot::RwLock::new(world));
        client
            .ecs
            .write()
            .entity_mut(client.entity)
            .insert(WorldHolder::new(client.entity, shared.clone()));
        (WorldRef::new(client), shared, partial)
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
        let (world_ref, _shared, _partial) = world_view(&[
            (BlockPos { x: 2, y: 0, z: 0 }, stone()),
            (BlockPos { x: -2, y: 0, z: 0 }, stone()),
        ]);
        // the predicate reads the world again — it runs with no world
        // lock held, so the re-read is safe
        let reread = WorldRef::new(world_ref.client().clone());
        let found = world_ref.find_blocks(
            &FindBlocksOpts::new(move |b: &Block| {
                reread.block_at(b.pos).is_some() && b.pos.x > 0 && b.state == stone()
            })
            .max_distance(16.)
            .count(10),
        );
        assert_eq!(found, vec![BlockPos { x: 2, y: 0, z: 0 }]);
    }

    /// A predicate that reads the world again must not deadlock, also when
    /// a writer waits on the world lock. With the lock held across the
    /// predicate, the queued writer blocks and the predicate's own re-read
    /// then blocks behind it (a `parking_lot` read lock is not re-entrant
    /// when a writer waits) — the scan never finishes.
    #[test]
    fn predicate_may_read_the_world_with_a_writer_queued() {
        let (world_ref, shared, _partial) = world_view(&[(BlockPos { x: 2, y: 0, z: 0 }, stone())]);
        let client = world_ref.client().clone();

        // the predicate tells the test when it first runs; the writer is
        // queued at that moment
        let (pred_tx, pred_rx) = std::sync::mpsc::channel::<()>();
        let probe = WorldRef::new(client.clone());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reread = WorldRef::new(client);
            let first = std::sync::atomic::AtomicBool::new(true);
            let found = probe.find_blocks(
                &FindBlocksOpts::new(move |b: &Block| {
                    if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        let _ = pred_tx.send(());
                        // let the test queue the writer while this predicate
                        // is still running
                        std::thread::sleep(std::time::Duration::from_millis(150));
                    }
                    reread.block_at(b.pos).is_some() && b.state == stone()
                })
                .max_distance(16.)
                .count(10),
            );
            let _ = done_tx.send(found);
        });
        pred_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("predicate never ran");
        let writer = std::thread::spawn(move || {
            let guard = shared.write();
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(guard);
        });
        let found = done_rx.recv_timeout(std::time::Duration::from_secs(5)).expect(
            "find_blocks deadlocked: the predicate's world read blocked behind the queued writer",
        );
        assert_eq!(found, vec![BlockPos { x: 2, y: 0, z: 0 }]);
        writer.join().unwrap();
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
            )
            .map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 0, z: 3 })
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
            )
            .is_none()
        );
    }

    pub(super) fn short_grass() -> BlockState {
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
            raycast_blocks(&world, from, dir, 10.).map(|(pos, _)| pos),
            Some(BlockPos { x: 0, y: 0, z: 3 })
        );
    }

    #[test]
    fn raycast_matcher_accepts_voxel_entry() {
        // Upstream `WorldSync.raycast`: with a matcher the first voxel
        // it accepts wins — no shape clip — so matchers reach
        // empty/partial shapes and intervening blocks never clip. The
        // matcher runs outside the world lock (`raycast_unlocked`).
        let (world_ref, _shared, _partial) = world_view(&[
            (BlockPos { x: 0, y: 0, z: 2 }, stone()),
            (BlockPos { x: 0, y: 0, z: 3 }, short_grass()),
        ]);
        let target = BlockPos { x: 0, y: 0, z: 3 };
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
            world_ref.raycast(from, dir, 10., None).map(|b| b.pos),
            Some(BlockPos { x: 0, y: 0, z: 2 })
        );
        // a matcher sails past the stone and accepts the grass on entry.
        let accept = BlockMatcher::Predicate(Box::new(move |b: &Block| b.pos == target));
        assert_eq!(
            world_ref
                .raycast(from, dir, 10., Some(&accept))
                .map(|b| b.pos),
            Some(target)
        );
        // a rejecting matcher must pass every block by.
        let reject = BlockMatcher::Predicate(Box::new(|_: &Block| false));
        assert!(world_ref.raycast(from, dir, 10., Some(&reject)).is_none());
    }

    #[test]
    fn can_see_block_partial_outline() {
        // The eye→voxel-centre ray never clips an empty-collision block
        // (short grass), yet `can_see_accepts` accepts it on voxel
        // entry — the case the outline-clip version false-negatived on.
        let (world_ref, shared, _partial) =
            world_view(&[(BlockPos { x: 0, y: 0, z: 3 }, short_grass())]);
        let target = BlockPos { x: 0, y: 0, z: 3 };
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
        let accept = BlockMatcher::Predicate(Box::new(move |b: &Block| {
            can_see_accepts(target, from, to, b.pos, b.state)
        }));
        // nothing clips (grass has no collision shape), but the target's
        // voxel is entered → accepted.
        assert!(world_ref.raycast(from, dir, range, None).is_none());
        assert_eq!(
            world_ref
                .raycast(from, dir, range, Some(&accept))
                .map(|b| b.pos),
            Some(target)
        );
        // an intervening block whose collision clips is accepted first
        // → the target is *not* seen through it.
        shared
            .write()
            .chunks
            .set_block_state(BlockPos { x: 0, y: 1, z: 2 }, stone());
        assert_eq!(
            world_ref
                .raycast(from, dir, range, Some(&accept))
                .map(|b| b.pos),
            Some(BlockPos { x: 0, y: 1, z: 2 })
        );
    }

    #[test]
    fn sign_lines_validates() {
        assert!(sign_lines("a\nb").is_ok());
        assert_eq!(sign_lines("a\nb").unwrap()[2], "");
        assert!(sign_lines("a\nb\nc\nd").is_ok());
        assert!(sign_lines("a\nb\nc\nd\ne").is_err());
        assert!(sign_lines(&"x".repeat(46)).is_err());
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

    /// Before `c_set_default_spawn_position` the spawn point is unknown,
    /// not the origin (B5).
    #[test]
    fn spawn_point_is_none_before_the_packet() {
        let bot = crate::test_util::bot(crate::test_util::despawned_client());
        assert_eq!(bot.spawn_point(), None);
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
