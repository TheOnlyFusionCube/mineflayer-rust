use bevy_ecs::message::{MessageCursor, Messages};
use mineflayer_client::{
    mining::{CancelMiningEvent, MiningFinished, MiningId, MiningOutcome, StartMiningBlockEvent},
    test_utils::prelude::*,
};
use mineflayer_core::position::{BlockPos, ChunkPos};
use mineflayer_protocol::packets::{ConnectionProtocol, game::ClientboundBlockUpdate};
use mineflayer_registry::builtin::BlockKind;

const A: BlockPos = BlockPos::new(0, 2, 0);
const B: BlockPos = BlockPos::new(1, 2, 0);

struct Mine {
    simulation: Simulation,
    cursor: MessageCursor<MiningFinished>,
    finished: Vec<(MiningId, MiningOutcome)>,
}

impl Mine {
    fn new() -> Self {
        let mut simulation = Simulation::new(ConnectionProtocol::Game);
        simulation.receive_packet(default_login_packet());
        simulation.receive_packet(make_basic_empty_chunk(ChunkPos::new(0, 0), (384 + 64) / 16));
        simulation.tick();
        // dirt breaks by hand in a few seconds, also in the air
        for (pos, kind) in [(A, BlockKind::Dirt), (B, BlockKind::Stone)] {
            simulation.receive_packet(ClientboundBlockUpdate {
                pos,
                block_state: kind.into(),
            });
        }
        simulation.tick();
        Self {
            simulation,
            cursor: MessageCursor::default(),
            finished: Vec::new(),
        }
    }

    fn start(&mut self, position: BlockPos) -> MiningId {
        let id = MiningId::next();
        let entity = self.simulation.entity;
        self.simulation.write_message(StartMiningBlockEvent {
            entity,
            position,
            force: true,
            id,
        });
        id
    }

    fn cancel(&mut self) {
        let entity = self.simulation.entity;
        self.simulation.trigger(CancelMiningEvent { entity });
        self.simulation.app.world_mut().flush();
        self.read();
    }

    fn tick(&mut self) {
        self.simulation.tick();
        self.read();
    }

    fn read(&mut self) {
        let messages = self
            .simulation
            .app
            .world()
            .resource::<Messages<MiningFinished>>();
        self.finished
            .extend(self.cursor.read(messages).map(|m| (m.id, m.outcome)));
    }
}

/// A start that a cancel drops before it is read ends `Cancelled`.
#[test]
fn a_dropped_start_is_cancelled() {
    let _lock = init();
    let mut mine = Mine::new();
    let id = mine.start(A);
    mine.cancel();
    mine.tick();
    assert_eq!(mine.finished, vec![(id, MiningOutcome::Cancelled)]);
}

/// A cancel while the block is mined ends the mining `Cancelled`.
#[test]
fn a_cancelled_mining_is_cancelled() {
    let _lock = init();
    let mut mine = Mine::new();
    let id = mine.start(A);
    mine.tick();
    assert!(mine.finished.is_empty());
    mine.cancel();
    assert_eq!(mine.finished, vec![(id, MiningOutcome::Cancelled)]);
}

/// A start for another block ends the current mining `Cancelled`.
#[test]
fn a_new_target_cancels_the_old_mining() {
    let _lock = init();
    let mut mine = Mine::new();
    let first = mine.start(A);
    mine.tick();
    let second = mine.start(B);
    mine.tick();
    assert_eq!(mine.finished, vec![(first, MiningOutcome::Cancelled)]);
    mine.cancel();
    assert_eq!(mine.finished[1], (second, MiningOutcome::Cancelled));
}

/// Mining to the end ends `Broken`, once.
#[test]
fn a_finished_mining_is_broken_once() {
    let _lock = init();
    let mut mine = Mine::new();
    let id = mine.start(A);
    for _ in 0..400 {
        mine.tick();
        if !mine.finished.is_empty() {
            break;
        }
    }
    for _ in 0..5 {
        mine.tick();
    }
    assert_eq!(mine.finished, vec![(id, MiningOutcome::Broken)]);
}
