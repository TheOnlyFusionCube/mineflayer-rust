//! A plugin written against the public API only: it keeps per-bot state,
//! runs on the game ticks, emits its own event type, and runs its action
//! as an operation that a newer start cancels.

use mineflayer::{
    Bot,
    engine::ecs::{self as bevy_ecs, component::Component},
    plugin::OpSlot,
    test_util::sim::BotSim,
};

#[derive(Component, Default)]
struct Counter {
    ticks: u32,
    op: OpSlot,
}

#[derive(Clone, Debug, PartialEq)]
struct Counted(u32);

trait CounterExt {
    /// Count `n` game ticks. `false` when a newer count or the end of the
    /// connection stopped it.
    fn count_ticks(&self, n: u32) -> impl Future<Output = bool> + Send;
}

impl CounterExt for Bot {
    async fn count_ticks(&self, n: u32) -> bool {
        let Some(op) = self.state_mut(|c: &mut Counter| self.start_op(&mut c.op)) else {
            return false;
        };
        let mut ticks = self.ticks();
        for _ in 0..n {
            tokio::select! {
                _ = op.cancelled() => return false,
                alive = ticks.next() => if !alive { return false },
            }
            let count = self.state_mut(|c: &mut Counter| {
                c.ticks += 1;
                c.ticks
            });
            self.emit_plugin_event(Counted(count.unwrap_or_default()));
        }
        self.state_mut(|c: &mut Counter| c.op.finish(&op))
            .unwrap_or(false)
    }
}

/// Run the engine one step, then let the plugin's task run.
fn step(sim: &mut BotSim, rt: &tokio::runtime::Runtime) {
    sim.tick();
    rt.block_on(async {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    });
}

#[test]
fn a_plugin_keeps_state_runs_on_ticks_and_emits_its_events() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut sim = BotSim::new();
    let mut counted = sim.bot.plugin_events::<Counted>();
    let bot = sim.bot.driver();
    let task = rt.spawn(async move { bot.count_ticks(3).await });
    step(&mut sim, &rt);
    for _ in 0..10 {
        if task.is_finished() {
            break;
        }
        step(&mut sim, &rt);
    }
    assert!(
        rt.block_on(task).unwrap(),
        "the count finished as its own op"
    );
    assert_eq!(sim.bot.state(|c: &Counter| c.ticks), Some(3));
    let got: Vec<Counted> = std::iter::from_fn(|| counted.try_recv().ok()).collect();
    assert_eq!(got, [Counted(1), Counted(2), Counted(3)]);
}

#[test]
fn a_new_start_cancels_the_running_op() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut sim = BotSim::new();
    let (first, second) = (sim.bot.driver(), sim.bot.driver());
    let first = rt.spawn(async move { first.count_ticks(1000).await });
    step(&mut sim, &rt);
    let second = rt.spawn(async move { second.count_ticks(1).await });
    for _ in 0..10 {
        step(&mut sim, &rt);
    }
    assert!(!rt.block_on(first).unwrap(), "the newer count cancelled it");
    assert!(rt.block_on(second).unwrap());
}
