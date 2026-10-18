//! The world-domain reducer: `WorldState` (spawn point, clocks, weather,
//! login metadata, block-update history) and the packet decode that
//! feeds it and raises the block, chest-lid, and piston events.

use super::*;
use mineflayer_engine::protocol::packets::game::{
    ClientboundBlockDestruction, ClientboundCustomPayload, ClientboundForgetLevelChunk,
    ClientboundLogin, ClientboundSectionBlocksUpdate, ClientboundSetTime,
};

/// Facade-kept world/game state, fed by [`observe_event`] inside the
/// bridge's event loop. Fields that the engine already owns (dimension,
/// gamemode) are read live off the client instead.
#[derive(Default)]
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
    /// `BlockState` last *reported* per updated position — the
    /// `blockUpdate` `oldBlock` source of truth. The bridge decodes
    /// `Event::Packet` only after the engine's `Update` pass finished (its
    /// `run_schedule_loop` holds the ECS write guard across the whole
    /// pass, and both `packet_listener` and
    /// `handle_block_update_event` run inside it — the async receiver
    /// can't interleave mid-pass), so the live world already shows the
    /// new state when we read `old`. Positions never seen updated fall
    /// back to that post-apply read (they report `old == new` — the
    /// best available without shadowing every loaded block). Cleared
    /// on login/respawn (dimension change) and per-position on chunk
    /// unload, bounded by [`MAX_TRACKED_UPDATES`].
    pub(super) last_block_updates: HashMap<BlockPos, BlockState>,
}

/// Bound on [`WorldState::last_block_updates`] — generous enough for a
/// busy redstone world; a full map clears wholesale on the next *new*
/// position (already-tracked positions keep their history — see
/// [`WorldState::update_and_old`]).
pub(super) const MAX_TRACKED_UPDATES: usize = 1 << 16;

impl WorldState {
    /// Record `new_state` as `pos`'s last-known state and return the
    /// previously recorded one — the `blockUpdate` `oldBlock` source of
    /// truth (see the field doc for why the live world can't answer).
    /// `None` means we never saw this position updated; the caller
    /// falls back to the post-apply world read.
    pub(super) fn update_and_old(
        &mut self,
        pos: BlockPos,
        new_state: BlockState,
    ) -> Option<BlockState> {
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
    state: &RwLock<WorldState>,
    client: &mineflayer_engine::Client,
    lids: &mut LidCounts,
    event: &Event,
) -> Vec<BotEvent> {
    match event {
        Event::Tick => {
            state.write().tick_clocks();
            vec![]
        }
        Event::Packet(packet) => decode_packet(client, state, lids, packet),
        _ => vec![],
    }
}

pub(super) fn decode_packet(
    client: &mineflayer_engine::Client,
    state: &RwLock<WorldState>,
    lids: &mut LidCounts,
    packet: &ClientboundGamePacket,
) -> Vec<BotEvent> {
    match packet {
        ClientboundGamePacket::BlockUpdate(p) => vec![block_update_event(
            client,
            &mut state.write(),
            lids,
            p.pos,
            p.block_state,
        )],
        ClientboundGamePacket::SectionBlocksUpdate(p) => {
            section_blocks_update(client, state, lids, p)
        }
        ClientboundGamePacket::ForgetLevelChunk(p) => forget_chunk(state, p),
        ClientboundGamePacket::SetTime(p) => set_time(client, state, p),
        ClientboundGamePacket::GameEvent(p) => state.write().apply_game_event(p.event, p.param),
        ClientboundGamePacket::ChangeDifficulty(p) => {
            state.write().difficulty = Some(p.difficulty);
            vec![]
        }
        ClientboundGamePacket::Login(p) => login(state, p),
        ClientboundGamePacket::Respawn(p) => {
            let mut state = state.write();
            state.is_flat = Some(p.common.is_flat);
            state.last_block_updates.clear();
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::SetDefaultSpawnPosition(p) => {
            state.write().spawn_point = Some(p.global_pos.clone());
            vec![BotEvent::GameStateChanged]
        }
        ClientboundGamePacket::CustomPayload(p) => custom_payload(state, p),
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

fn section_blocks_update(
    client: &mineflayer_engine::Client,
    state: &RwLock<WorldState>,
    lids: &mut LidCounts,
    p: &ClientboundSectionBlocksUpdate,
) -> Vec<BotEvent> {
    let mut out = Vec::with_capacity(p.states.len());
    // one write guard across the batch — every entry updates the
    // tracker, so sequential updates to the same position see the
    // section's own earlier state as `old` (matching the packet
    // order the server applied them in).
    let mut state = state.write();
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

fn forget_chunk(state: &RwLock<WorldState>, p: &ClientboundForgetLevelChunk) -> Vec<BotEvent> {
    // A tracked position whose chunk unloads mustn't keep its
    // history — after a reload the world knows the real state,
    // and reporting the stale pre-unload value as `old` is worse
    // than the `old == new` fallback.
    state
        .write()
        .last_block_updates
        .retain(|pos, _| pos.x >> 4 != p.pos.x || pos.z >> 4 != p.pos.z);
    vec![BotEvent::ChunkUnload(p.pos)]
}

fn set_time(
    client: &mineflayer_engine::Client,
    state: &RwLock<WorldState>,
    p: &ClientboundSetTime,
) -> Vec<BotEvent> {
    {
        let mut state = state.write();
        state.game_time = p.game_time;
        // lock rule: the world-state lock is held while
        // `get_component`/`with_registry_holder` take brief ECS
        // locks — allowed direction.
        // `with_registry_holder` panics when the local player has
        // been torn down (it reaches `Client::world` internally) —
        // a `set_time` racing a disconnect must not take the client
        // thread down with it
        if client.get_component::<WorldHolder>().is_some() {
            client.with_registry_holder(|registries| {
                for (clock, clock_state) in &p.clock_updates {
                    if let Some(key) = clock.key_owned(registries) {
                        state.clocks.insert(key.into_ident(), clock_state.clone());
                    }
                }
            });
        }
    }
    vec![BotEvent::Time]
}

fn login(state: &RwLock<WorldState>, p: &ClientboundLogin) -> Vec<BotEvent> {
    {
        let mut state = state.write();
        state.hardcore = Some(p.hardcore);
        state.max_players = Some(p.max_players);
        state.is_flat = Some(p.common.is_flat);
        // a new world means the tracked states describe stale
        // positions — drop them rather than report them as `old`
        state.last_block_updates.clear();
    }
    vec![BotEvent::GameStateChanged]
}

fn custom_payload(state: &RwLock<WorldState>, p: &ClientboundCustomPayload) -> Vec<BotEvent> {
    // `minecraft:brand` → bot.game.serverBrand.
    if p.identifier.to_string() == "minecraft:brand"
        && let Ok(brand) = String::mc_read(&mut Cursor::new(&p.data.0[..]))
    {
        state.write().server_brand = Some(brand);
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
    state: &mut WorldState,
    lids: &mut LidCounts,
    pos: BlockPos,
    new_state: BlockState,
) -> BotEvent {
    // The engine's `handle_block_update_event` drains the queued updates in
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

pub(super) fn block_event(
    client: &mineflayer_engine::Client,
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

    use mineflayer_engine::{
        core::entity_id::MinecraftEntityId,
        protocol::packets::game::c_block_destruction::ClientboundBlockDestruction,
    };

    use super::*;
    use crate::world::tests::*;

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
        use mineflayer_engine::protocol::packets::game::{
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
            let mut s = state.write();
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
            let s = state.read();
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
        assert!(state.read().is_raining);
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
        let clock = &state.read().clocks[&Identifier::from("minecraft:overworld")];
        assert_eq!(clock.total_ticks, 101);
        assert!((clock.partial_tick - 0.9).abs() < f32::EPSILON);
        // non-packet events decode to nothing
        assert!(observe_event(&state, &client, &mut lids, &Event::Login).is_empty());
    }

    /// `handle_block_update_event` applies the update inside the
    /// same `Update` pass that emits `Event::Packet`, and the bridge
    /// can't observe mid-pass — so a live world read can't supply
    /// `oldBlock`. The tracked update history must: a re-updated
    /// position reports its last-reported state even though the world
    /// already shows the new one.
    #[test]
    fn block_update_old_comes_from_update_history() {
        use mineflayer_engine::protocol::packets::game::c_block_update::ClientboundBlockUpdate;

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

        // Simulate the engine's real ordering: `handle_block_update_event`
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
}
