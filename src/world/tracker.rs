//! The world-domain reducer: `WorldState` (spawn point, clocks, weather,
//! login metadata) and the packet decode that feeds it and raises the
//! block, chest-lid, and piston events.

use std::collections::VecDeque;

use mineflayer_engine::{
    block_update::QueuedServerBlockUpdates,
    ecs::{
        self as bevy_ecs,
        prelude::{Commands, Component, Entity, Query},
    },
    protocol::packets::game::{
        ClientboundBlockDestruction, ClientboundCustomPayload, ClientboundSetTime,
    },
};
use tracing::debug;

use super::*;
use crate::bot::with_component_mut;

/// World and game state the engine drops, on the client entity. The
/// bridge's [`observe_event`] writes it; the `Bot` world methods read it.
/// Fields that the engine already owns (dimension, gamemode) are read live
/// off the client instead.
#[derive(Component, Default)]
pub(crate) struct WorldState {
    /// `bot.spawnPoint` — from `c_set_default_spawn_position` (dropped
    /// by the engine).
    pub(super) spawn_point: Option<GlobalPos>,
    /// Per-dimension clocks from `c_set_time`, advanced one tick per
    /// game tick like vanilla does.
    pub(super) clocks: HashMap<Identifier, ClockState>,
    /// `game_time` from `c_set_time` — the world's age.
    pub(super) game_time: u64,
    pub(super) is_raining: bool,
    pub(super) rain_state: f32,
    pub(super) thunder_state: f32,
    pub(super) difficulty: Option<Difficulty>,
    pub(super) hardcore: Option<bool>,
    pub(super) max_players: Option<i32>,
    pub(super) is_flat: Option<bool>,
    pub(super) server_brand: Option<String>,
}

/// The state each queued server block update replaced, in queue order.
/// [`record_replaced_blocks`] appends during the engine pass; the bridge
/// takes one entry per block update it decodes after the pass.
#[derive(Component, Default)]
pub(crate) struct ReplacedBlocks(VecDeque<(BlockPos, Option<BlockState>)>);

/// Records the state each queued block update will replace. It runs after
/// the engine applied this pass's chunks and before it applies the block
/// updates, so a second update of one position in one pass replaces the
/// first one's state, as upstream's `oldBlock` does.
pub(crate) fn record_replaced_blocks(
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &QueuedServerBlockUpdates,
        &WorldHolder,
        Option<&mut ReplacedBlocks>,
    )>,
) {
    for (entity, queued, holder, replaced) in &mut query {
        if queued.list.is_empty() {
            continue;
        }
        let world = holder.shared.read();
        let mut pending: HashMap<BlockPos, BlockState> = HashMap::new();
        let entries = queued.list.iter().map(|&(pos, new)| {
            let old = pending
                .insert(pos, new)
                .or_else(|| world.chunks.get_block_state(pos));
            (pos, old)
        });
        match replaced {
            Some(mut replaced) => replaced.0.extend(entries),
            None => {
                let entries = ReplacedBlocks(entries.collect());
                commands.entity(entity).insert(entries);
            }
        }
    }
}

/// The state the next decoded update of `pos` replaced. Entries for
/// updates the bridge never decoded are dropped on the way.
fn take_replaced(client: &mineflayer_engine::Client, pos: BlockPos) -> Option<Option<BlockState>> {
    with_component_mut::<ReplacedBlocks, _>(client, |replaced| {
        while let Some((at, old)) = replaced.0.pop_front() {
            if at == pos {
                return Some(old);
            }
        }
        None
    })
    .flatten()
}

impl WorldState {
    /// Vanilla clock tick: `partial_tick += rate; total_ticks += floor`.
    pub(super) fn tick_clocks(&mut self) {
        for clock in self.clocks.values_mut() {
            clock.partial_tick += clock.rate;
            let full = clock.partial_tick.floor();
            clock.partial_tick -= full;
            clock.total_ticks = clock.total_ticks.wrapping_add(full as u64);
        }
    }

    /// Port of mineflayer's rain.js `game_state_change` handling.
    pub(super) fn apply_game_event(&mut self, event: EventType, param: f32) -> Vec<BotEvent> {
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

    pub(super) fn set_raining(&mut self, is_raining: bool, out: &mut Vec<BotEvent>) {
        if self.is_raining == is_raining {
            return;
        }
        self.is_raining = is_raining;
        out.push(BotEvent::Rain);
    }
}

/// Upstream's `block2` position: populated only for a 'right' chest
/// half — the left-cardinal neighbour that completes a double chest
/// (`FACING_MAP`). 'single' chests carry no `block2` ('left' halves are
/// swallowed by the caller before this runs).
pub(super) fn chest_other_half_pos(
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
    pub(super) fn update(&mut self, pos: BlockPos, open_count: u8) -> bool {
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

/// Feed one engine [`Event`] into the world domain — the packet decode of
/// mineflayer's `plugins/blocks.js` + `time.js` + `rain.js` + `game.js`.
/// The engine drops or internalizes these packets, so the bridge runs this
/// synchronously on every event (same loop position as the entity and
/// window observers): derived events keep packet order like upstream's
/// in-handler emits, and nothing rides a second broadcast hop that could
/// lag or drop.
///
/// `lids` is the bridge-kept `openCountByPos` dedup map — one per
/// connection, like upstream's.
pub(crate) fn observe_event(
    client: &mineflayer_engine::Client,
    lids: &mut LidCounts,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        Event::Tick => {
            with_component_mut::<WorldState, _>(client, WorldState::tick_clocks);
            vec![]
        }
        Event::Packet(packet) => {
            super::block_entity::observe(client, packet);
            decode_packet(client, lids, packet)
        }
        _ => vec![],
    }
}

/// Change the world state on the client entity; no events.
fn update(client: &mineflayer_engine::Client, f: impl FnOnce(&mut WorldState)) -> Vec<BotEvent> {
    with_component_mut::<WorldState, _>(client, f);
    vec![]
}

pub(super) fn decode_packet(
    client: &mineflayer_engine::Client,
    lids: &mut LidCounts,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::BlockUpdate(p) => {
            vec![block_update_event(client, lids, p.pos, p.block_state)]
        }
        ClientboundGamePacket::SectionBlocksUpdate(p) => p
            .states
            .iter()
            .map(|s| block_update_event(client, lids, p.section_pos + s.pos, s.state))
            .collect(),
        ClientboundGamePacket::ForgetLevelChunk(p) => vec![BotEvent::ChunkUnload(p.pos)],
        ClientboundGamePacket::SetTime(p) => set_time(client, p),
        ClientboundGamePacket::GameEvent(p) => {
            with_component_mut::<WorldState, _>(client, |s| s.apply_game_event(p.event, p.param))
                .unwrap_or_default()
        }
        ClientboundGamePacket::ChangeDifficulty(p) => {
            update(client, |s| s.difficulty = Some(p.difficulty))
        }
        ClientboundGamePacket::Login(p) => {
            update(client, |s| {
                s.hardcore = Some(p.hardcore);
                s.max_players = Some(p.max_players);
                s.is_flat = Some(p.common.is_flat);
            });
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::Respawn(p) => {
            update(client, |s| s.is_flat = Some(p.common.is_flat));
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::SetDefaultSpawnPosition(p) => {
            update(client, |s| s.spawn_point = Some(p.global_pos.clone()));
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::CustomPayload(p) => custom_payload(client, p),
        ClientboundGamePacket::OpenSignEditor(p) => vec![BotEvent::SignOpen {
            block: block_at(client, p.pos),
            is_front_text: p.is_front_text,
        }],
        ClientboundGamePacket::BlockEntityData(p) => {
            vec![BotEvent::BlockEntityData(p.clone())]
        }
        ClientboundGamePacket::BlockDestruction(p) => block_destruction(client, p),
        ClientboundGamePacket::BlockEvent(p) => block_event(client, lids, p),
        _ => vec![],
    }
}

fn set_time(client: &mineflayer_engine::Client, p: &ClientboundSetTime) -> Vec<BotEvent> {
    // `with_registry_holder` panics when the local player has been torn
    // down (it reaches `Client::world` internally) — a `set_time` racing
    // a disconnect must not take the client thread down with it
    let mut clocks = Vec::new();
    if client.get_component::<WorldHolder>().is_some() {
        client.with_registry_holder(|registries| {
            for (clock, clock_state) in &p.clock_updates {
                if let Some(key) = clock.key_owned(registries) {
                    clocks.push((key.into_ident(), clock_state.clone()));
                }
            }
        });
    }
    update(client, |s| {
        s.game_time = p.game_time;
        s.clocks.extend(clocks);
    });
    vec![BotEvent::Time]
}

fn custom_payload(
    client: &mineflayer_engine::Client,
    p: &ClientboundCustomPayload,
) -> Vec<BotEvent> {
    // `minecraft:brand` → bot.game.serverBrand.
    if p.identifier.to_string() == "minecraft:brand"
        && let Ok(brand) = String::mc_read(&mut Cursor::new(&p.data.0[..]))
    {
        update(client, |s| s.server_brand = Some(brand));
    }
    vec![]
}

fn block_destruction(
    client: &mineflayer_engine::Client,
    p: &ClientboundBlockDestruction,
) -> Vec<BotEvent> {
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

pub(super) fn block_update_event(
    client: &mineflayer_engine::Client,
    lids: &mut LidCounts,
    pos: BlockPos,
    new_state: BlockState,
) -> BotEvent {
    // the engine applied the update before the bridge runs, so the old
    // state comes from what `record_replaced_blocks` saw before the apply
    let old = take_replaced(client, pos).unwrap_or_else(|| {
        debug!(?pos, "no recorded old state for a block update");
        with_world(client, |w| w.get_block_state(pos)).flatten()
    });
    if old.map(|s| s.as_block_kind()) != Some(new_state.as_block_kind()) {
        // upstream drops the lid count when the block type changes, and
        // the block entity goes with the block
        lids.0.remove(&pos);
        super::block_entity::forget(client, pos);
    }
    BotEvent::BlockUpdate {
        old: old.map(|state| Block::new(client, pos, state)),
        new: Block::new(client, pos, new_state),
    }
}

pub(super) fn block_event(
    client: &mineflayer_engine::Client,
    lids: &mut LidCounts,
    p: &ClientboundBlockEvent,
) -> Vec<BotEvent> {
    // upstream emits 'noteHeard' with the block position only, so it does
    // not wait for the block to be in a loaded chunk
    if p.block == BlockKind::NoteBlock {
        return vec![BotEvent::NoteHeard {
            block: p.pos,
            instrument: crate::misc::Instrument::from(p.action_id),
            pitch: p.action_parameter,
        }];
    }
    let Some(block) = block_at(client, p.pos) else {
        return vec![];
    };
    match p.block {
        BlockKind::Piston | BlockKind::StickyPiston => {
            use mineflayer_engine::core::direction::Direction;
            let direction = match p.action_parameter {
                0 => Direction::Down,
                1 => Direction::Up,
                2 => Direction::North,
                3 => Direction::South,
                4 => Direction::West,
                5 => Direction::East,
                _ => return vec![],
            };
            vec![BotEvent::PistonMove {
                block,
                is_pulling: p.action_id != 0,
                direction,
            }]
        }
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
mod tests;
