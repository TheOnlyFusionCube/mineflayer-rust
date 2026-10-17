use mineflayer_client::{
    mining::{CancelMiningEvent, Mining, MiningQueued, StartMiningBlockEvent},
    test_utils::prelude::*,
};
use mineflayer_core::position::{BlockPos, ChunkPos};
use mineflayer_protocol::packets::{
    ConnectionProtocol,
    game::{ClientboundBlockUpdate, ServerboundGamePacket, s_player_action},
};
use mineflayer_registry::builtin::BlockKind;

const POS: BlockPos = BlockPos::new(0, 2, 0);

fn with_stone() -> (Simulation, SentPackets) {
    let mut simulation = Simulation::new(ConnectionProtocol::Game);
    let sent_packets = SentPackets::new(&mut simulation);
    simulation.receive_packet(default_login_packet());
    simulation.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
    simulation.tick();
    simulation.receive_packet(ClientboundBlockUpdate {
        pos: POS,
        block_state: BlockKind::Stone.into(),
    });
    simulation.tick();
    (simulation, sent_packets)
}

fn start(simulation: &mut Simulation) {
    let entity = simulation.entity;
    simulation.write_message(StartMiningBlockEvent {
        entity,
        position: POS,
        force: true,
    });
}

fn cancel(simulation: &mut Simulation) {
    let entity = simulation.entity;
    simulation.trigger(CancelMiningEvent { entity });
    simulation.app.world_mut().flush();
}

fn player_actions(sent_packets: &SentPackets) -> Vec<s_player_action::Action> {
    let mut actions = Vec::new();
    while let Some(packet) = sent_packets.next() {
        if let ServerboundGamePacket::PlayerAction(p) = packet {
            actions.push(p.action);
        }
    }
    actions
}

/// A cancel drops a start that is queued but not read yet: the start
/// message was written before the cancel.
#[test]
fn cancel_drops_a_start_written_before_it() {
    let _lock = init();
    let (mut simulation, sent_packets) = with_stone();
    sent_packets.clear();

    start(&mut simulation);
    cancel(&mut simulation);
    simulation.tick();
    simulation.tick();

    assert!(!simulation.has_component::<MiningQueued>());
    assert!(!simulation.has_component::<Mining>());
    assert_eq!(player_actions(&sent_packets), vec![]);
}

/// A start written after a cancel is not affected by it.
#[test]
fn cancel_keeps_a_start_written_after_it() {
    let _lock = init();
    let (mut simulation, sent_packets) = with_stone();
    sent_packets.clear();

    cancel(&mut simulation);
    start(&mut simulation);
    simulation.tick();

    assert!(simulation.has_component::<Mining>());
    assert_eq!(
        player_actions(&sent_packets),
        vec![s_player_action::Action::StartDestroyBlock]
    );
}

/// A cancel while the block is being mined aborts it, like vanilla
/// `stopDestroyBlock`; a cancel with nothing to cancel does nothing.
#[test]
fn cancel_aborts_live_mining_and_is_a_no_op_when_idle() {
    let _lock = init();
    let (mut simulation, sent_packets) = with_stone();
    cancel(&mut simulation);
    simulation.tick();
    start(&mut simulation);
    simulation.tick();
    assert!(simulation.has_component::<Mining>());
    sent_packets.clear();

    cancel(&mut simulation);
    assert!(!simulation.has_component::<Mining>());
    assert_eq!(
        player_actions(&sent_packets),
        vec![s_player_action::Action::AbortDestroyBlock]
    );

    cancel(&mut simulation);
    simulation.tick();
    assert_eq!(player_actions(&sent_packets), vec![]);
}
