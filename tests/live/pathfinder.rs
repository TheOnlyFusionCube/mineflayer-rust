//! Live pathfinder test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566), spawns, runs `pathfinder.goto`
//! a few blocks over and verifies the goal-reached event and arrival,
//! then interrupts a longer path with `stop()`. Only compiled with
//! `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1 (single `live` binary)
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::{collect_until, horizontal_distance};
use azalea::{BlockPos, Vec3};
use mineflayer::{Bot, BotEvent, Goal};
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Collect every event into a shared buffer on a background task. The
/// broadcast channel floods with `Packet`/`Move` events (thousands per
/// second on a busy world), so a receiver that isn't drained continuously
/// lags past everything — assertions must run against a live-drained
/// buffer, not a subscribe-then-read-later one.
fn spawn_collector(mut rx: Receiver<BotEvent>) -> std::sync::Arc<std::sync::Mutex<Vec<BotEvent>>> {
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let writer = buf.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) => writer.lock().unwrap().push(e),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
    buf
}

/// Wait until `buf` contains an event matching `pred`, or panic.
async fn wait_in_buf(
    buf: &std::sync::Mutex<Vec<BotEvent>>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if buf.lock().unwrap().iter().any(&pred) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out after {timeout:?} waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A spot the bot can actually stand on, using azalea's own predicates
/// (`is_block_state_standable`/`is_block_state_passable`) so foliage,
/// water and thin shapes are classified exactly the way the pathfinder
/// classifies them: solid/slab/stair ground below, passable feet + head.
fn is_standable(bot: &Bot, feet: BlockPos) -> bool {
    use azalea::pathfinder::world::{is_block_state_passable, is_block_state_standable};
    bot.block_at(feet.down(1))
        .is_some_and(|b| is_block_state_standable(b.state))
        && bot
            .block_at(feet)
            .is_some_and(|b| is_block_state_passable(b.state))
        && bot
            .block_at(feet.up(1))
            .is_some_and(|b| is_block_state_passable(b.state))
}

/// Flood-fill outward from the bot's feet over walkable cells (standable,
/// reachable in ≤1-block steps, jump headroom on the way up), and collect
/// up to `count` cells at least `min_dist` away (Manhattan). Targets come
/// back in BFS order, so the first few span different directions — each
/// is reachable by plain walking, so `goto` can't pick a spot behind a
/// wall, in water, or at the bottom of a pit.
fn reachable_targets(bot: &Bot, pos: Vec3, min_dist: i32, count: usize) -> Vec<BlockPos> {
    use std::collections::{HashSet, VecDeque};

    let start = BlockPos::from(pos);
    let mut visited = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    let mut out = Vec::new();
    while let Some(node) = queue.pop_front() {
        if visited.len() > 3000 || out.len() >= count {
            break;
        }
        if node.y.abs_diff(start.y) <= 1
            && (node.x - start.x).abs() + (node.z - start.z).abs() >= min_dist
        {
            out.push(node);
        }
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            // a step can land one block up, level, or one block down;
            // don't let the search wander into deep pits or up cliffs
            for dy in [-1, 0, 1] {
                let next = BlockPos::new(node.x + dx, node.y + dy, node.z + dz);
                if next.y.abs_diff(start.y) > 2
                    || !visited.insert(next)
                    || !is_standable(bot, next)
                    // stepping up needs jump headroom over the source cell
                    || (dy == 1
                        && !bot
                            .block_at(node.up(2))
                            .is_some_and(|b| b.bounding_box() == mineflayer::BoundingBox::Empty))
                {
                    continue;
                }
                queue.push_back(next);
            }
        }
    }
    out
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn pathfinder() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u7bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();
    collect_until(
        &mut rx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "spawn",
    )
    .await;
    // let the world/chunks finish arriving so block_at sees real terrain
    bot.wait_for_ticks(20).await.expect("world settle");

    let pf = bot.pathfinder();
    assert!(!pf.have_goal());
    assert!(!pf.is_moving());

    // — goto: path to a reachable block a few steps away —
    // Normal-terrain flakiness (debug A* slowness, an awkward pocket the
    // executor wanders into) means one candidate may stall — walk the
    // candidates until one completes; the assertions below always run
    // against the goal that actually finished.
    let candidates = reachable_targets(&bot, bot.position(), 5, 4);
    assert!(
        !candidates.is_empty(),
        "no walkable spot near spawn — is the fixture world loaded?"
    );
    let events = spawn_collector(bot.events());
    let mut target = None;
    for cand in &candidates {
        let attempt =
            tokio::time::timeout(Duration::from_secs(25), pf.goto(Goal::Block(*cand))).await;
        if let Ok(Ok(())) = attempt {
            target = Some(*cand);
            break;
        }
        pf.stop();
        eprintln!("goto to {cand} didn't finish ({attempt:?}), trying next candidate");
    }
    let target = target.unwrap_or_else(|| {
        let pos = bot.position();
        let feet = BlockPos::from(pos);
        let mut around = Vec::new();
        for dy in -1..=1 {
            for dx in -1..=1 {
                for dz in -1..=1 {
                    let p = BlockPos::new(feet.x + dx, feet.y + dy, feet.z + dz);
                    around.push(format!(
                        "{p}: {}",
                        bot.block_at(p).map_or("?".into(), |b| b.name().to_string())
                    ));
                }
            }
        }
        panic!(
            "goto failed on every reachable candidate {candidates:?}; \
             bot at {pos} (feet {feet}), surroundings: {around:?}"
        );
    });
    // goto can resolve off the same tick that emits goal_reached — give
    // the bridge one tick to land it before asserting; the payload must
    // be the goal we actually set
    wait_in_buf(
        &events,
        Duration::from_secs(3),
        |e| matches!(e, BotEvent::GoalReached { goal: Goal::Block(p) } if *p == target),
        "goal_reached for our target",
    )
    .await;

    // the event stream carries the whole lifecycle, in order
    let events = events.lock().unwrap().clone();
    let pf_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                BotEvent::GoalUpdated { .. }
                    | BotEvent::PathUpdate { .. }
                    | BotEvent::GoalReached { .. }
                    | BotEvent::PathStop { .. }
                    | BotEvent::PathReset { .. }
            )
        })
        .collect();
    let goal_updated = pf_events.iter().position(|e| {
        matches!(e, BotEvent::GoalUpdated { goal: Goal::Block(p), dynamic: false } if *p == target)
    });
    let path_update = pf_events
        .iter()
        .position(|e| matches!(e, BotEvent::PathUpdate { path, .. } if !path.is_empty()));
    let goal_reached = pf_events
        .iter()
        .position(|e| matches!(e, BotEvent::GoalReached { .. }));
    assert!(
        goal_updated.is_some() && path_update.is_some() && goal_reached.is_some(),
        "expected goal_updated, path_update and goal_reached; got {pf_events:?} (of {} total)",
        events.len()
    );
    assert!(goal_updated < path_update && path_update < goal_reached);

    // the bot actually arrived and the goal is gone
    let arrived = bot.position();
    let target_center = Vec3::new(
        target.x as f64 + 0.5,
        target.y as f64,
        target.z as f64 + 0.5,
    );
    assert!(
        horizontal_distance(arrived, target_center) < 1.6,
        "expected to be at {target_center}, at {arrived}"
    );
    assert!(!pf.is_moving());
    assert!(!pf.have_goal());
    assert!(pf.goal().is_none());

    // — stop: interrupt a longer path mid-run —
    // fresh-seed terrain can leave the bot in a pocket; let the world
    // settle first so the walkability scan sees loaded chunks
    bot.wait_for_ticks(10)
        .await
        .expect("settle before stop scan");
    let events = spawn_collector(bot.events());
    let far = reachable_targets(&bot, bot.position(), 12, 1)
        .into_iter()
        .next()
        .or_else(|| {
            reachable_targets(&bot, bot.position(), 8, 1)
                .into_iter()
                .next()
        })
        .or_else(|| {
            reachable_targets(&bot, bot.position(), 6, 1)
                .into_iter()
                .next()
        })
        .or_else(|| {
            reachable_targets(&bot, bot.position(), 4, 1)
                .into_iter()
                .next()
        })
        .unwrap_or_else(|| {
            // degenerate pocket: aim at a fixed offset and let the
            // pathfinder try; `moved` below still guards the assertion
            let feet = bot.position().to_block_pos_floor();
            eprintln!("no walkable spot for the stop test; aiming at a fixed offset");
            BlockPos::new(feet.x + 12, feet.y, feet.z)
        });
    pf.set_goal(Goal::XZ { x: far.x, z: far.z }, false)
        .expect("set_goal failed");
    // it takes a tick or two to calculate and start executing
    let mut moved = false;
    for _ in 0..60 {
        if pf.is_moving() {
            moved = true;
            break;
        }
        bot.wait_for_ticks(1).await.expect("wait for path start");
    }
    assert!(moved, "path never started executing");
    assert!(pf.have_goal());

    let stopped_at = bot.position();
    pf.stop();
    wait_in_buf(
        &events,
        Duration::from_secs(5),
        |e| {
            matches!(
                e,
                BotEvent::PathStop {
                    goal: Some(Goal::XZ { .. })
                }
            )
        },
        "path_stop carrying the XZ goal",
    )
    .await;
    assert!(!pf.is_moving());
    assert!(!pf.have_goal());
    let seen = events.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|e| matches!(e, BotEvent::GoalUpdated { .. })),
        "stop should follow a goal_updated; got {seen:?}"
    );

    // halted means the position barely changes afterwards
    bot.wait_for_ticks(40).await.expect("drift check");
    assert!(
        horizontal_distance(stopped_at, bot.position()) < 1.5,
        "bot kept moving after stop(): {} -> {}",
        stopped_at,
        bot.position()
    );

    // — a satisfied goal reports reached without going anywhere —
    // (BlockPosGoal checks the bot's feet block, so our own block is it)
    let here = BlockPos::from(bot.position());
    let events2 = spawn_collector(bot.events());
    tokio::time::timeout(Duration::from_secs(10), pf.goto(Goal::Block(here)))
        .await
        .expect("satisfied goto timed out")
        .expect("satisfied goto failed");
    wait_in_buf(
        &events2,
        Duration::from_secs(3),
        |e| matches!(e, BotEvent::GoalReached { goal: Goal::Block(p) } if *p == here),
        "satisfied goal_reached",
    )
    .await;

    bot.quit(None);
}
