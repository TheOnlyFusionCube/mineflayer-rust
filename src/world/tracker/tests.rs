
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
