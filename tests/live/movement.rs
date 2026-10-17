//! Live movement test — connects to the local offline-mode fixture
//! (`scripts/live-server.sh`, port 25566), spawns, walks forward via
//! control states, looks at a point, and verifies events + stopping.
//! Only compiled with `--features live`; run it with:
//!
//! ```sh
//! bash scripts/live-server.sh && cargo test --features live -- --ignored --test-threads=1 (single `live` binary)
//! ```

#![cfg(feature = "live")]

use std::time::Duration;

use crate::common::{collect_until, horizontal_distance};
use mineflayer::{BlockPos, Bot, BotEvent, Control, Vec3};
use mineflayer_engine::physics::collision::BlockWithShape as _;

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
    assert!(bot.control_states().forward);
    collect_until(
        &mut rx,
        Duration::from_secs(3),
        |e| matches!(e, BotEvent::Move { .. }),
        "move",
    )
    .await;
    bot.wait_for_ticks(60).await.expect("wait_for_ticks");
    // The world spawn is in a bamboo jungle: stalks have collision, grow,
    // and spread between runs, so some headings are walled off. Sweep 8
    // headings, the ones with free collision shapes along the first two
    // blocks first, and judge each by its own progress (E5).
    let free = |pos: BlockPos| {
        bot.block_at(pos)
            .is_some_and(|b| b.state.collision_shape().is_empty())
    };
    let clear = |angle: f64| {
        let feet = bot.position();
        (1..=2).all(|step| {
            let p = Vec3::new(
                feet.x + f64::from(step) * angle.cos(),
                feet.y,
                feet.z + f64::from(step) * angle.sin(),
            )
            .to_block_pos_floor();
            free(p) && free(p.up(1))
        })
    };
    let mut headings: Vec<u8> = (0..8).collect();
    headings.sort_by_key(|h| !clear(f64::from(*h) * std::f64::consts::FRAC_PI_4));
    let mut walked = horizontal_distance(start, bot.position()) > 1.0;
    let mut tries = Vec::new();
    for heading in headings {
        if walked {
            break;
        }
        let base = bot.position();
        let angle = f64::from(heading) * std::f64::consts::FRAC_PI_4;
        let target = Vec3::new(base.x + 5. * angle.cos(), base.y, base.z + 5. * angle.sin());
        bot.look_at(target, false).await.expect("look_at failed");
        bot.wait_for_ticks(30).await.expect("wait_for_ticks");
        let progress = horizontal_distance(base, bot.position());
        tries.push((heading, progress));
        walked = progress > 1.0;
    }
    assert!(
        walked,
        "bot never walked on any heading from {start:?}: (heading, progress) {tries:?}"
    );
    // Stop before the look checks: a bot that keeps walking into terrain
    // can get a server position correction, which carries a rotation and
    // replaces the look under test (E5 series 4: 2 runs in 10).
    bot.clear_control_states();
    bot.wait_for_ticks(10).await.expect("settle");

    // look_at turns the head toward the point (east, slightly above eye).
    let base = bot.position();
    let target = base.east(5.0).up(2.0);
    bot.look_at(target, false).await.expect("look_at failed");
    let dir = bot.client().direction();
    // atan2(-5, 0) = -pi/2 upstream radians -> 270 notchian degrees
    // (the engine stores it unwrapped, -90 == 270).
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

    // Clearing the states stops movement. Walk again first, then clear.
    // The bot can't be asserted to a dead standstill — residual momentum,
    // gravity, and water currents are outside control states — so let it
    // settle, then check it's no longer moving at anything near walking
    // speed.
    bot.set_control_state(Control::Forward, true);
    bot.wait_for_ticks(10).await.expect("walk");
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
