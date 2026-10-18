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
    let mut lids = LidCounts::default();
    // the breaker id resolves through our own entity's index — once it
    // is despawned the lookup must miss, not panic.
    let out = decode_packet(
        &client,
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

    let client = crate::test_util::live_entity_client(());
    let mut lids = LidCounts::default();
    let packet = |p| Event::Packet(Arc::new(p));

    // the events come out in the order of the packets, and the state is
    // updated by the same call
    let out = observe_event(
        &client,
        &mut lids,
        &packet(ClientboundGamePacket::ForgetLevelChunk(
            ClientboundForgetLevelChunk {
                pos: ChunkPos::new(3, 4),
            },
        )),
    );
    assert!(matches!(&out[..], [BotEvent::ChunkUnload(_)]));
    let out = observe_event(
        &client,
        &mut lids,
        &packet(ClientboundGamePacket::GameEvent(ClientboundGameEvent {
            event: EventType::StartRaining,
            param: 0.,
        })),
    );
    assert!(matches!(&out[..], [BotEvent::Rain]));
    assert_eq!(
        crate::bot::with_component::<WorldState, _>(&client, |s| s.is_raining),
        Some(true)
    );
}

#[test]
fn observe_event_ticks_clocks_on_tick() {
    let client = crate::test_util::live_entity_client(WorldState {
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
    assert!(observe_event(&client, &mut lids, &Event::Tick).is_empty());
    let clock = crate::bot::with_component::<WorldState, _>(&client, |s| {
        s.clocks[&Identifier::from("minecraft:overworld")].clone()
    })
    .unwrap();
    assert_eq!(clock.total_ticks, 101);
    assert!((clock.partial_tick - 0.9).abs() < f32::EPSILON);
    // non-packet events decode to nothing
    assert!(observe_event(&client, &mut lids, &Event::Login).is_empty());
}

/// Upstream reads `oldBlock` before it writes the new state, so even the
/// first update of a position reports what was there.
#[test]
fn the_first_update_of_a_position_reports_the_old_block() {
    use mineflayer_engine::protocol::packets::game::c_block_update::ClientboundBlockUpdate;

    let mut sim = crate::test_util::sim::BotSim::new();
    sim.events();
    let pos = BlockPos::new(1, 2, 3);
    sim.receive(ClientboundBlockUpdate {
        pos,
        block_state: stone(),
    });
    sim.tick();
    let updates: Vec<_> = sim
        .events()
        .into_iter()
        .filter_map(|e| match e {
            BotEvent::BlockUpdate { old, new } => Some((old.map(|b| b.state), new.state)),
            _ => None,
        })
        .collect();
    assert_eq!(updates, [(Some(BlockState::AIR), stone())]);
}

/// Two updates of one position in one pass: the second's `old` is the
/// first's `new`, in packet order.
#[test]
fn updates_of_one_position_in_one_pass_chain_their_old_blocks() {
    use mineflayer_engine::{
        core::position::{ChunkSectionBlockPos, ChunkSectionPos},
        protocol::packets::game::c_section_blocks_update::{
            BlockStateWithPosition, ClientboundSectionBlocksUpdate,
        },
    };

    let mut sim = crate::test_util::sim::BotSim::new();
    sim.events();
    let dirt = BlockState::from(BlockKind::Dirt);
    let at = ChunkSectionBlockPos { x: 1, y: 1, z: 1 };
    sim.receive(ClientboundSectionBlocksUpdate {
        section_pos: ChunkSectionPos { x: 0, y: 0, z: 0 },
        states: vec![
            BlockStateWithPosition {
                pos: at,
                state: stone(),
            },
            BlockStateWithPosition {
                pos: at,
                state: dirt,
            },
        ],
    });
    sim.tick();
    let updates: Vec<_> = sim
        .events()
        .into_iter()
        .filter_map(|e| match e {
            BotEvent::BlockUpdate { old, new } => Some((old.map(|b| b.state), new.state)),
            _ => None,
        })
        .collect();
    assert_eq!(
        updates,
        [(Some(BlockState::AIR), stone()), (Some(stone()), dirt)]
    );
}
