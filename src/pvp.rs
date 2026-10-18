//! The mineflayer-pvp plugin surface: `bot.pvp()` — a background
//! follow+attack loop.
//!
//! `attack(entity)` installs a dynamic [`Goal::Follow`] (the same
//! mechanism `pvp`'s upstream `GoalFollow(target, followRange)` uses)
//! and spawns a per-tick driver: while the target is loaded, alive, and
//! inside `view_distance`, the bot looks at it and swings on the
//! `melee_attack_rate` schedule whenever it's within `attack_range`.
//! `stop()`, `force_stop()`, the target dying/despawning/leaving view,
//! or disconnect ends the loop, clears the follow goal, and emits
//! [`BotEvent::StoppedAttacking`] exactly once (whoever ends the loop
//! owns the report).

use mineflayer_engine::{
    EntityRef,
    attack::get_attack_strength_delay,
    ecs::{self as bevy_ecs, prelude::Component},
    entity::{Attributes, Position, dimensions::EntityDimensions, metadata::Health},
};

use crate::bot::{TickStream, with_component, with_component_mut};
use crate::{
    Bot, BotEvent, Error, Result,
    op::{Op, OpSlot},
    pathfinder::{Goal, Movements},
};

/// mineflayer-pvp `meleeAttackRate` — the `TimingSolver` pick that
/// paces swings.
///
/// `getTicks` is sampled after each attack (or each out-of-range
/// attempt) for how many ticks to wait before the next one; upstream's
/// default `MaxDamageOffset(-2, 2)` is [`AttackRate::CooldownOffset`].
#[derive(Clone, Debug)]
pub enum AttackRate {
    /// `RandomTicks(min, max)` — wait a uniformly-random integer in
    /// `[min, max)` ticks between swings (upstream defaults `10..20`).
    /// The weapon cooldown is ignored, like upstream's solver.
    RandomTicks {
        /// Inclusive lower bound.
        min: u32,
        /// Exclusive upper bound; `<= min` collapses to `min`.
        max: u32,
    },
    /// `MaxDamageOffset(min, max)` — the held weapon's attack cooldown
    /// (the engine's `get_attack_strength_delay`, the vanilla
    /// `20 / attack_speed` upstream's `getCooldown` reads from mcdata)
    /// plus a uniformly-random offset in `[min, max)` ticks. This is
    /// the upstream default (`-2..2`).
    CooldownOffset {
        /// Inclusive lower bound.
        min: i32,
        /// Exclusive upper bound; `<= min` collapses to `min`.
        max: i32,
    },
}

impl Default for AttackRate {
    /// Upstream's `new MaxDamageOffset()`: `min = -2`, `max = 2`.
    fn default() -> Self {
        Self::CooldownOffset { min: -2, max: 2 }
    }
}

impl AttackRate {
    /// Sample `getTicks(bot)` — upstream clamps every solver to ≥1
    /// tick.
    fn ticks(&self, cooldown_ticks: i64, rng: &mut u64) -> i64 {
        match *self {
            Self::RandomTicks { min, max } => rand_i64(rng, min as i64, max as i64).max(1),
            Self::CooldownOffset { min, max } => {
                (rand_i64(rng, i64::from(min), i64::from(max)) + cooldown_ticks).max(1)
            }
        }
    }
}

/// xorshift64* — the driver's jitter source for [`AttackRate`]
/// sampling (a full `rand` dependency would buy nothing here).
fn rand_next(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// A uniformly-ish random integer in `[min, max)` — `min` when the
/// range is empty, matching `Math.floor(rand * (max - min) + min)`.
fn rand_i64(state: &mut u64, min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    min + (rand_next(state) % (max - min) as u64) as i64
}

/// The tunable surface upstream `PVP` exposes as mutable fields —
/// stored on [`PvpState`] so the `Pvp<'a>` borrowed view can mutate it
/// through the ECS.
#[derive(Clone)]
struct PvpOpts {
    /// `followRange` — how close the follow goal gets (upstream 2).
    follow_range: f32,
    /// `viewDistance` — beyond this the target counts as defeated
    /// (upstream 128).
    view_distance: f64,
    /// `attackRange` — swing distance (upstream 3.0).
    attack_range: f64,
    /// `meleeAttackRate` — the swing pacer.
    rate: AttackRate,
    /// `movements` — applied to the pathfinder on `attack`, like
    /// upstream (`None` leaves the pathfinder's config alone).
    movements: Option<Movements>,
}

impl Default for PvpOpts {
    fn default() -> Self {
        Self {
            follow_range: 2.,
            view_distance: 128.,
            attack_range: 3.,
            rate: AttackRate::default(),
            movements: Some(Movements::default()),
        }
    }
}

/// Facade state the engine has no slot for: the pvp target, the running
/// attack loop, and the option fields.
/// Stored as an ECS component created on first use — rule 2 of the
/// storage rule on `bot::BotShared`.
#[derive(Component, Default)]
struct PvpState {
    /// The entity `attack` is driving at; `bot.pvp.target`.
    target: Option<EntityRef>,
    /// The running attack loop; every `attack` starts a new one.
    op: OpSlot,
    /// The upstream field surface (`followRange`, `viewDistance`, …).
    opts: PvpOpts,
}

/// Stop pathfinding only while our follow goal is still the installed
/// one — a `set_goal` since `attack` (from the user, or another plugin)
/// belongs to them and `stop()` mustn't kill it.
fn stop_follow_if_ours(bot: &Bot, target: &EntityRef) {
    let ours = matches!(
        bot.pathfinder().goal(),
        Some(Goal::Follow { entity, .. }) if entity.id() == target.id()
    );
    if ours {
        bot.pathfinder().stop();
    }
}

/// The `bot.pvp` namespace — a borrowed view onto the [`Bot`], like
/// [`Bot::pathfinder`].
pub struct Pvp<'a> {
    bot: &'a Bot,
}

impl Bot {
    /// `bot.pvp` — the mineflayer-pvp namespace (`attack`, `stop`,
    /// `target`).
    pub fn pvp(&self) -> Pvp<'_> {
        Pvp { bot: self }
    }
}

impl Pvp<'_> {
    /// `bot.pvp.attack(entity)` — start (or retarget) the attack loop and
    /// return immediately, like upstream. The bot follows `entity` while
    /// it stays loaded, alive, and inside `view_distance`, swinging on
    /// the `melee_attack_rate` schedule whenever `attack_range` allows.
    /// Attacking the already-active target is a silent no-op; a second
    /// `attack` on a different target retargets the loop. The loop ends
    /// on [`Pvp::stop`], the target's death/despawn/leaving view, or
    /// disconnect — each ending emits [`BotEvent::StoppedAttacking`]
    /// exactly once. Emits [`BotEvent::StartedAttacking`] on every
    /// successful non-no-op call.
    ///
    /// # Errors
    ///
    /// [`Error::NotConnected`] before the bot has spawned.
    pub fn attack(&self, entity: &EntityRef) -> Result<()> {
        let bot = self.bot;
        if !bot.logged_in() {
            return Err(Error::NotConnected);
        }
        // upstream `if (target === this.target) return;` — no goal reset,
        // no new driver, no `startedAttacking`
        let current =
            with_component::<PvpState, _>(&bot.client, |s| s.target.as_ref().map(|t| t.id()))
                .flatten();
        if current == Some(entity.id()) {
            return Ok(());
        }
        let opts =
            with_component::<PvpState, _>(&bot.client, |s| s.opts.clone()).unwrap_or_default();
        // upstream: `if (this.movements) pathfinder.setMovements(this.movements)`
        if let Some(movements) = opts.movements {
            bot.pathfinder().set_movements(movements);
        }
        // set the goal first — if it fails nothing was recorded
        bot.pathfinder().set_goal(
            Goal::Follow {
                entity: entity.clone(),
                range: opts.follow_range,
            },
            true,
        )?;
        // a retarget cancels the previous loop, which then exits silently
        let started = with_component_mut::<PvpState, _>(&bot.client, |s| {
            s.target = Some(entity.clone());
            s.op.start(&bot.root)
        });
        let Some(op) = started else {
            // the client entity vanished between logged_in and here —
            // unwind the goal we just set
            bot.pathfinder().stop();
            return Err(Error::NotConnected);
        };
        tokio::spawn(drive(bot.driver_handle(), entity.clone(), op));
        bot.emit_event(BotEvent::StartedAttacking);
        Ok(())
    }

    /// `bot.pvp.stop()` — end the attack loop. Emits
    /// [`BotEvent::StoppedAttacking`] and clears the follow goal when an
    /// attack was running; a no-op otherwise.
    pub fn stop(&self) {
        let stopped = self.take_target();
        let Some(target) = stopped else {
            return;
        };
        stop_follow_if_ours(self.bot, &target);
        self.bot.emit_event(BotEvent::StoppedAttacking);
    }

    /// `bot.pvp.forceStop()` — end the attack loop and clear the
    /// pathfinder goal unconditionally (upstream's
    /// `pathfinder.setGoal(null)`): unlike [`Pvp::stop`], this also
    /// drops a goal someone else installed after `attack`. Emits
    /// [`BotEvent::StoppedAttacking`] when an attack was running; a
    /// no-op otherwise.
    pub fn force_stop(&self) {
        if self.take_target().is_none() {
            return;
        }
        self.bot.pathfinder().stop();
        self.bot.emit_event(BotEvent::StoppedAttacking);
    }

    /// `bot.pvp.target` — the entity the loop is driving at, `None` when
    /// idle.
    pub fn target(&self) -> Option<EntityRef> {
        with_component::<PvpState, _>(&self.bot.client, |s| s.target.clone()).flatten()
    }

    /// `bot.pvp.followRange` — how close the follow goal gets (default 2).
    pub fn follow_range(&self) -> f32 {
        self.opts(|o| o.follow_range)
    }
    /// Set `followRange`; applies to the next `attack` (the installed
    /// follow goal keeps its range, like upstream's GoalFollow).
    pub fn set_follow_range(&self, range: f32) {
        self.set_opts(|o| o.follow_range = range);
    }
    /// `bot.pvp.viewDistance` — beyond this the target counts as
    /// defeated and the loop stops (default 128). Read live each tick.
    pub fn view_distance(&self) -> f64 {
        self.opts(|o| o.view_distance)
    }
    /// Set `viewDistance`.
    pub fn set_view_distance(&self, distance: f64) {
        self.set_opts(|o| o.view_distance = distance);
    }
    /// `bot.pvp.attackRange` — swing distance (default 3.0). Read live
    /// each tick.
    pub fn attack_range(&self) -> f64 {
        self.opts(|o| o.attack_range)
    }
    /// Set `attackRange`.
    pub fn set_attack_range(&self, range: f64) {
        self.set_opts(|o| o.attack_range = range);
    }
    /// `bot.pvp.meleeAttackRate` — the swing pacer (default
    /// [`AttackRate::CooldownOffset`] `-2..2`, upstream's
    /// `MaxDamageOffset`). Read live each tick.
    pub fn melee_attack_rate(&self) -> AttackRate {
        self.opts(|o| o.rate.clone())
    }
    /// Set `meleeAttackRate`.
    pub fn set_melee_attack_rate(&self, rate: AttackRate) {
        self.set_opts(|o| o.rate = rate);
    }
    /// `bot.pvp.movements` — applied to the pathfinder on `attack`
    /// (default `Some(Movements::default())`; `None` leaves the
    /// pathfinder's config alone, like assigning `null` upstream).
    pub fn movements(&self) -> Option<Movements> {
        self.opts(|o| o.movements.clone())
    }
    /// Set `movements`.
    pub fn set_movements(&self, movements: Option<Movements>) {
        self.set_opts(|o| o.movements = movements);
    }

    /// Clear the target and cancel the loop; the target when an attack
    /// was running. Shared by `stop`/`force_stop`.
    fn take_target(&self) -> Option<EntityRef> {
        with_component_mut::<PvpState, _>(&self.bot.client, |s| {
            s.op.cancel();
            s.target.take()
        })
        .flatten()
    }

    /// Read an opts field through the component (defaults pre-spawn).
    fn opts<R>(&self, f: impl FnOnce(&PvpOpts) -> R) -> R {
        let opts =
            with_component::<PvpState, _>(&self.bot.client, |s| s.opts.clone()).unwrap_or_default();
        f(&opts)
    }
    /// Mutate an opts field; a silent no-op pre-spawn (there's no state
    /// to attach it to yet).
    fn set_opts(&self, f: impl FnOnce(&mut PvpOpts)) {
        with_component_mut::<PvpState, _>(&self.bot.client, |s| f(&mut s.opts));
    }
}

/// The attack loop: one iteration per game tick. Owns the
/// `StoppedAttacking` report only when it ends on its own (target gone,
/// or disconnect — `stop()` and `attack`-retargets already reported or
/// made the loop stale); a stale or cancelled driver exits silently so
/// the event can never double-fire.
///
/// `op` ends on a retarget, a `stop()`, or a disconnect; only the last
/// leaves the outcome to the loop, which then reports the end.
async fn drive(bot: Bot, entity: EntityRef, op: Op) {
    let mut ticks = TickStream::new(&bot.client, op.token.clone());
    // upstream `timeToNextAttack` — attack() resets it to 0, each tick
    // decrements, and `=== -1` attempts a swing
    let mut time_to_next_attack: i64 = 0;
    let mut was_in_range = false;
    let mut rng = seed_rng(&entity);
    while ticks.next().await {
        // copy what we need out and drop the component guards before any
        // other ECS access — a read guard held across another lock
        // deadlocks behind a queued writer
        let target = {
            let pos = entity.get_component::<Position>().map(|p| **p);
            let health = entity.get_component::<Health>().map(|h| h.0);
            let height = entity
                .get_component::<EntityDimensions>()
                .map(|d| f64::from(d.height));
            (pos, health, height)
        };
        let gone = match target {
            // despawned, or dead (upstream's `isValid` check ends the same
            // way — a dying entity keeps its position through the death
            // animation, so health gates it)
            (Some(_), Some(h), _) if h <= 0. => true,
            (None, _, _) => true,
            _ => false,
        };
        if gone {
            report_end_if_ours(&bot, &op);
            return;
        }
        let Some(target_pos) = target.0 else {
            return;
        };
        // opts are read live each tick, like upstream's per-tick field
        // reads — mid-fight retunes take effect immediately
        let (view_distance, attack_range, rate) = with_component::<PvpState, _>(&bot.client, |s| {
            (
                s.opts.view_distance,
                s.opts.attack_range,
                s.opts.rate.clone(),
            )
        })
        .unwrap_or_else(|| {
            let d = PvpOpts::default();
            (d.view_distance, d.attack_range, d.rate)
        });
        // upstream `checkRange`: past `viewDistance` the target counts
        // as defeated — `stop()` ends the loop (and reports it)
        let dist = bot.position().distance_to(target_pos);
        if dist > view_distance {
            report_end_if_ours(&bot, &op);
            return;
        }
        let in_range = dist <= attack_range;
        // upstream resets the wait to 0 on the enter-range edge — the
        // first swing lands on the next tick
        if !was_in_range && in_range {
            time_to_next_attack = 0;
        }
        was_in_range = in_range;
        time_to_next_attack -= 1;
        if time_to_next_attack != -1 {
            continue;
        }
        if !in_range {
            // upstream resamples without swinging when out of range
            time_to_next_attack = rate.ticks(cooldown_ticks(&bot), &mut rng);
            continue;
        }
        // aim at the entity's torso, like upstream's offset(0, height, 0)
        let _ = bot
            .look_at(target_pos.up(target.2.unwrap_or(0.9)), false)
            .await;
        bot.attack(&entity, true);
        bot.emit_event(BotEvent::AttackedTarget);
        time_to_next_attack = rate.ticks(cooldown_ticks(&bot), &mut rng);
    }
    report_end_if_ours(&bot, &op);
}

/// The held weapon's cooldown in ticks — the `getCooldown(heldItem)`
/// upstream's `MaxDamageOffset` adds its offset to. `0` when the bot
/// has no `Attributes` (a despawned client — the driver exits on the
/// next state check anyway).
fn cooldown_ticks(bot: &Bot) -> i64 {
    bot.client
        .get_component::<Attributes>()
        .map(|a| get_attack_strength_delay(&a).ceil() as i64)
        .unwrap_or(0)
}

/// Seed the jitter source from the target — different drivers mustn't
/// share one stream, and a per-driver seed keeps `AttackRate` sampling
/// independent of when `attack` ran.
fn seed_rng(entity: &EntityRef) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    (nanos ^ u64::from(entity.id().index_u32()).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1
}

/// Report the loop's end only when the state is still ours: clear the
/// target, release the follow goal, and emit `StoppedAttacking`. A
/// `stop()`/retarget that beat the driver here already owns the outcome,
/// so a stale driver exits silently.
fn report_end_if_ours(bot: &Bot, op: &Op) {
    let ended = with_component_mut::<PvpState, _>(&bot.client, |s| {
        if s.op.finish(op) {
            s.target.take()
        } else {
            None
        }
    });
    if let Some(target) = ended.flatten() {
        stop_follow_if_ours(bot, &target);
        bot.emit_event(BotEvent::StoppedAttacking);
    }
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{Vec3, registry::identifier::Identifier, world::WorldName};

    use super::*;
    use crate::test_util::{despawned_client, live_entity_client, tick_client};

    /// A bot whose client entity is alive and carries a `WorldName` —
    /// enough for `Bot::logged_in` to pass. No systems run on this ECS,
    /// so the spawned driver/follow tasks die immediately (no
    /// `TickBroadcast` resource) — the synchronous state transitions are
    /// what's under test here.
    fn spawned_bot() -> Bot {
        crate::test_util::bot(live_entity_client((
            WorldName(Identifier::from("minecraft:overworld")),
            Position::new(Vec3::new(0., 64., 0.)),
        )))
    }

    /// A bot whose ECS carries a `TickBroadcast` — the driver's
    /// `get_tick_broadcaster()` parks instead of panicking (no systems
    /// run, so ticks only arrive when the test sends them), matching how
    /// it waits on a live connection.
    fn tick_bot() -> Bot {
        crate::test_util::bot(tick_client((
            WorldName(Identifier::from("minecraft:overworld")),
            Position::new(Vec3::new(0., 64., 0.)),
        )))
    }

    /// Spawn an attackable-looking entity (positioned, so the driver
    /// would gate on range) and get its ref.
    fn spawn_target(bot: &Bot, x: f64, z: f64) -> EntityRef {
        let entity = bot
            .client
            .ecs
            .write()
            .spawn(Position::new(Vec3::new(x, 64., z)))
            .id();
        bot.client.entity_ref_for(entity)
    }

    /// Drain every pending event; report whether `pred` matched one.
    fn saw(
        rx: &mut tokio::sync::broadcast::Receiver<BotEvent>,
        pred: fn(&BotEvent) -> bool,
    ) -> bool {
        let mut saw = false;
        while let Ok(event) = rx.try_recv() {
            saw |= pred(&event);
        }
        saw
    }

    /// Drain every pending event; report whether a `StoppedAttacking`
    /// was among them.
    fn saw_stopped_attacking(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> bool {
        saw(rx, |e| matches!(e, BotEvent::StoppedAttacking))
    }

    #[tokio::test]
    async fn attack_then_stop_transitions_and_reports_once() {
        let bot = spawned_bot();
        let mut rx = bot.events();
        let target = spawn_target(&bot, 5., 0.);

        // start → engaged: the loop records its target and reports it
        bot.pvp().attack(&target).unwrap();
        assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));
        assert!(saw(&mut rx, |e| matches!(e, BotEvent::StartedAttacking)));

        // stop → idle: target cleared, the follow goal ours to remove,
        // and the stopped event emitted
        bot.pvp().stop();
        assert!(bot.pvp().target().is_none());
        assert!(bot.pathfinder().goal().is_none());
        assert!(saw_stopped_attacking(&mut rx));

        // stopping again is a silent no-op — no state, no event
        let mut rx = bot.events();
        bot.pvp().stop();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn retarget_swaps_without_reporting_a_stop() {
        let bot = spawned_bot();
        let mut rx = bot.events();
        let first = spawn_target(&bot, 5., 0.);
        let second = spawn_target(&bot, -5., 0.);

        bot.pvp().attack(&first).unwrap();
        bot.pvp().attack(&second).unwrap();
        assert_eq!(bot.pvp().target().map(|t| t.id()), Some(second.id()));
        // a retarget never stopped the loop — no StoppedAttacking, but
        // upstream's startedAttacking fires per attack() call
        let mut started = 0;
        while let Ok(event) = rx.try_recv() {
            started += usize::from(matches!(event, BotEvent::StartedAttacking));
            assert!(!matches!(event, BotEvent::StoppedAttacking));
        }
        assert_eq!(started, 2);

        bot.pvp().stop();
    }

    /// upstream `PVP.ts`: `if (target === this.target) return;` — attacking
    /// the already-active target is a silent no-op (no fresh goal, no
    /// second `StartedAttacking`).
    #[tokio::test]
    async fn attack_same_target_is_a_no_op() {
        let bot = spawned_bot();
        let mut rx = bot.events();
        let target = spawn_target(&bot, 5., 0.);

        bot.pvp().attack(&target).unwrap();
        bot.pvp().attack(&target).unwrap();
        assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));
        let mut started = 0;
        while let Ok(event) = rx.try_recv() {
            started += usize::from(matches!(event, BotEvent::StartedAttacking));
        }
        assert_eq!(started, 1);

        bot.pvp().stop();
    }

    #[tokio::test]
    async fn force_stop_clears_a_foreign_goal_unlike_stop() {
        let bot = spawned_bot();
        let mut rx = bot.events();
        let target = spawn_target(&bot, 5., 0.);
        let other = spawn_target(&bot, -5., 0.);
        bot.pvp().attack(&target).unwrap();

        // a follow goal on a *different* entity installed after
        // attack() isn't ours — stop() leaves it running, force_stop()
        // clears it like upstream's `setGoal(null)`
        bot.pathfinder()
            .set_goal(
                Goal::Follow {
                    entity: other.clone(),
                    range: 1.,
                },
                true,
            )
            .unwrap();
        bot.pvp().stop();
        assert!(matches!(
            bot.pathfinder().goal(),
            Some(Goal::Follow { entity, .. }) if entity.id() == other.id()
        ));

        bot.pvp().attack(&target).unwrap();
        bot.pathfinder()
            .set_goal(
                Goal::Follow {
                    entity: other.clone(),
                    range: 1.,
                },
                true,
            )
            .unwrap();
        bot.pvp().force_stop();
        assert!(bot.pvp().target().is_none());
        assert!(bot.pathfinder().goal().is_none());
        assert!(saw_stopped_attacking(&mut rx));

        // idle force_stop is a no-op
        let mut rx = bot.events();
        bot.pvp().force_stop();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn tunables_round_trip_and_feed_the_follow_goal() {
        let bot = spawned_bot();
        // upstream defaults
        assert_eq!(bot.pvp().view_distance(), 128.);
        assert_eq!(bot.pvp().attack_range(), 3.);
        assert_eq!(bot.pvp().follow_range(), 2.);
        assert!(matches!(
            bot.pvp().melee_attack_rate(),
            AttackRate::CooldownOffset { min: -2, max: 2 }
        ));
        assert!(bot.pvp().movements().is_some());

        bot.pvp().set_view_distance(64.);
        bot.pvp().set_attack_range(2.5);
        bot.pvp().set_follow_range(1.5);
        bot.pvp()
            .set_melee_attack_rate(AttackRate::RandomTicks { min: 5, max: 8 });
        bot.pvp().set_movements(None);
        assert_eq!(bot.pvp().view_distance(), 64.);
        assert_eq!(bot.pvp().attack_range(), 2.5);
        assert_eq!(bot.pvp().follow_range(), 1.5);
        assert!(matches!(
            bot.pvp().melee_attack_rate(),
            AttackRate::RandomTicks { min: 5, max: 8 }
        ));
        assert!(bot.pvp().movements().is_none());

        // follow_range reaches the installed goal
        let target = spawn_target(&bot, 5., 0.);
        bot.pvp().attack(&target).unwrap();
        assert!(matches!(
            bot.pathfinder().goal(),
            Some(Goal::Follow { range, .. }) if range == 1.5
        ));
        bot.pvp().stop();
    }

    #[test]
    fn attack_rate_samples_in_range_and_clamps() {
        let mut rng = 0x1234_5678_9abc_def1;
        for _ in 0..256 {
            let t = AttackRate::RandomTicks { min: 10, max: 20 }.ticks(0, &mut rng);
            assert!((10..20).contains(&t));
            // cooldown + offset, clamped ≥1 like upstream's getTicks
            let t = AttackRate::CooldownOffset { min: -2, max: 2 }.ticks(13, &mut rng);
            assert!((11..15).contains(&t));
        }
        // a negative offset can pull the sum below 1 — upstream clamps
        let mut rng = 0xdead_beef;
        assert_eq!(
            AttackRate::CooldownOffset { min: -10, max: -5 }.ticks(3, &mut rng),
            1
        );
        // empty ranges collapse to min
        let mut rng = 1;
        assert_eq!(
            AttackRate::RandomTicks { min: 7, max: 7 }.ticks(0, &mut rng),
            7
        );
    }

    /// A target beyond `view_distance` counts as defeated — the driver
    /// ends the run the way `stop()` would (upstream `checkRange`).
    #[tokio::test]
    async fn target_past_view_distance_ends_the_loop_and_reports() {
        let bot = tick_bot();
        let mut rx = bot.events();
        // well past the default 128 — no components for the driver to
        // attack, but Position is enough for the distance check
        let target = spawn_target(&bot, 200., 0.);
        let op = with_component_mut::<PvpState, _>(&bot.client, |s| {
            s.target = Some(target.clone());
            s.op.start(&bot.root)
        })
        .unwrap();
        let sender = crate::test_util::tick_sender(&bot.client);
        let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));

        // a broadcast send lands only for receivers already subscribed —
        // let the driver park on its tick receiver first
        while sender.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
        sender.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), driver)
            .await
            .expect("driver kept chasing a defeated target")
            .unwrap();
        assert!(bot.pvp().target().is_none());
        assert!(saw_stopped_attacking(&mut rx));
    }

    /// A live `view_distance` retune takes effect on the very next tick
    /// — upstream reads the field each physics tick.
    #[tokio::test]
    async fn view_distance_retune_ends_a_running_loop() {
        let bot = tick_bot();
        let target = spawn_target(&bot, 50., 0.);
        let op = with_component_mut::<PvpState, _>(&bot.client, |s| {
            s.target = Some(target.clone());
            s.op.start(&bot.root)
        })
        .unwrap();
        let sender = crate::test_util::tick_sender(&bot.client);
        let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));

        // inside the default 128 the loop keeps running
        bot.pvp().set_view_distance(10.);
        // a broadcast send lands only for receivers already subscribed —
        // let the driver park on its tick receiver first
        while sender.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
        sender.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), driver)
            .await
            .expect("driver ignored a live view_distance retune")
            .unwrap();
        assert!(bot.pvp().target().is_none());
    }

    #[tokio::test]
    async fn attack_before_spawn_is_not_connected() {
        let client = despawned_client();
        let bot = crate::test_util::bot(client.clone());
        let target = client.entity_ref_for(client.entity);
        assert!(matches!(
            bot.pvp().attack(&target),
            Err(Error::NotConnected)
        ));
        assert!(bot.pvp().target().is_none());
    }

    /// Disconnect retires the driver: the tick channel can't close on its
    /// own (the driver's `Client` keeps the `TickBroadcast` sender alive),
    /// so the root token, which the bridge cancels on exit, is the only
    /// signal.
    #[tokio::test]
    async fn disconnect_ends_the_loop_and_reports() {
        let bot = tick_bot();
        let mut rx = bot.events();
        let target = spawn_target(&bot, 5., 0.);
        let op = with_component_mut::<PvpState, _>(&bot.client, |s| {
            s.target = Some(target.clone());
            s.op.start(&bot.root)
        })
        .unwrap();
        let driver = tokio::spawn(drive(bot.clone(), target.clone(), op));
        // parked on ticks like a live connection — target still recorded
        assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));

        // the bridge's exit
        bot.root.cancel();

        tokio::time::timeout(std::time::Duration::from_secs(5), driver)
            .await
            .expect("driver outlived the connection")
            .unwrap();
        assert!(bot.pvp().target().is_none());
        assert!(saw_stopped_attacking(&mut rx));
    }
}
