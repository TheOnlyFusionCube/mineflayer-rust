//! Live movement test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566), spawns, walks forward via
//! control states, looks at a point, and verifies events + stopping.
//! Only compiled with `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live --test live_movement -- --ignored
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use mineflayer::{Bot, BotEvent, Control};
use tokio::sync::broadcast::{Receiver, error::RecvError};

/// Drain events until `pred` matches, returning every event consumed —
/// the caller can assert on the whole stream (e.g. ForcedMove arriving on
/// the way to Spawn), not just the matched event.
async fn collect_until(
    rx: &mut Receiver<BotEvent>,
    timeout: Duration,
    pred: impl Fn(&BotEvent) -> bool,
    what: &str,
) -> Vec<BotEvent> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => {
                let done = pred(&event);
                seen.push(event);
                if done {
                    return seen;
                }
            }
            Ok(Err(RecvError::Lagged(_))) => {}
            Ok(Err(RecvError::Closed)) => {
                panic!("event stream closed while waiting for {what}")
            }
            Err(_) => panic!("timed out after {timeout:?} waiting for {what}"),
        }
    }
}

fn horizontal_distance(a: azalea::Vec3, b: azalea::Vec3) -> f64 {
    let (dx, dz) = (a.x - b.x, a.z - b.z);
    (dx * dx + dz * dz).sqrt()
}

#[tokio::test]
#[ignore = "requires the live fixture server on localhost:25566"]
async fn movement() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("u5bot")
        .connect()
        .await
        .expect("connect failed");
    let mut rx = bot.events();

    // Spawn carries the server's position packet -> ForcedMove on the way.
    let seen = collect_until(
        &mut rx,
        Duration::from_secs(15),
        |e| matches!(e, BotEvent::Spawn),
        "spawn",
    )
    .await;
    assert!(
        seen.iter().any(|e| matches!(e, BotEvent::ForcedMove)),
        "expected ForcedMove from the spawn position packet, got {seen:?}"
    );
    assert_eq!(bot.username(), "u5bot");
    assert!(bot.physics_enabled());

    // Hold forward+jump ~3s: a `move` event streams and the bot really
    // walks. The default worldgen spawn can face a slope or water — held
    // jump climbs 1-block steps and treads water, and 3s covers slow media.
    let start = bot.position();
    bot.set_control_state(Control::Forward, true);
    bot.set_control_state(Control::Jump, true);
    assert!(bot.control_state(Control::Forward));
    assert!(bot.control_state(Control::Jump));
    assert!(bot.jump_queued());
    assert_eq!(bot.control_states().forward, true);
    collect_until(
        &mut rx,
        Duration::from_secs(3),
        |e| matches!(e, BotEvent::Move { .. }),
        "move",
    )
    .await;
    bot.wait_for_ticks(60).await.expect("wait_for_ticks");
    let moved = bot.position();
    assert!(
        horizontal_distance(start, moved) > 1.0,
        "bot should have walked forward: {start:?} -> {moved:?}"
    );
    bot.set_control_state(Control::Jump, false);

    // look_at turns the head toward the point (east, slightly above eye).
    let base = bot.position();
    let target = base.east(5.0).up(2.0);
    bot.look_at(target, false).await.expect("look_at failed");
    let dir = bot.client().direction();
    // atan2(-5, 0) = -pi/2 upstream radians -> 270 notchian degrees
    // (azalea stores it unwrapped, -90 == 270).
    let yaw_off = (dir.y_rot() - 270.0).rem_euclid(360.0);
    assert!(
        yaw_off < 3.0 || yaw_off > 357.0,
        "expected yaw ~270 looking east, got {}",
        dir.y_rot()
    );
    // dy = 2 - eye_height(1.62) -> pitch ~ -4.4 degrees.
    assert!(
        (dir.x_rot() + 4.4).abs() < 3.0,
        "expected pitch ~-4.4, got {}",
        dir.x_rot()
    );

    // Clearing the states stops movement. The bot can't be asserted to a
    // dead standstill — residual momentum, gravity, and water currents are
    // outside control states — so let it settle, then check it's no longer
    // moving at anything near walking speed.
    bot.clear_control_states();
    let states = bot.control_states();
    assert_eq!(states, mineflayer::ControlState::default());
    bot.wait_for_ticks(10).await.expect("settle");
    let stopped_at = bot.position();
    bot.wait_for_ticks(10).await.expect("settle");
    assert!(
        horizontal_distance(stopped_at, bot.position()) < 0.8,
        "bot kept walking after clear_control_states"
    );

    bot.quit(None);
}
