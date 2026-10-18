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
//! [`PvpEvent::StoppedAttacking`] exactly once (whoever ends the loop
//! owns the report).

use mineflayer::{
    Bot, Error, Goal, Movements, Result,
    engine::{
        EntityRef,
        attack::get_attack_strength_delay,
        ecs::{self as bevy_ecs, prelude::Component},
        entity::{Attributes, Position, dimensions::EntityDimensions, metadata::Health},
    },
    plugin::{Op, OpSlot},
};

/// The pvp plugin's events; subscribe with
/// [`Bot::plugin_events::<PvpEvent>`](Bot::plugin_events).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PvpEvent {
    /// mineflayer-pvp `startedAttacking` — `pvp().attack()` engaged a
    /// target. Fires on every successful `attack` call including
    /// retargets; upstream emits `stoppedAttacking` + `startedAttacking`
    /// on a retarget — ours keeps the one run alive and fires only this.
    StartedAttacking,
    /// mineflayer-pvp `stoppedAttacking` — the attack loop ended
    /// (explicit `stop`, target died/despawned). No payload, like
    /// upstream.
    StoppedAttacking,
    /// mineflayer-pvp `attackedTarget` — the driver swung at the target
    /// this tick. No payload, like upstream.
    AttackedTarget,
}

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
    fn ticks(&self, cooldown_ticks: i64) -> i64 {
        match *self {
            Self::RandomTicks { min, max } => random_in(min as i64, max as i64).max(1),
            Self::CooldownOffset { min, max } => {
                (random_in(i64::from(min), i64::from(max)) + cooldown_ticks).max(1)
            }
        }
    }
}

/// A random integer in `[min, max)` — `min` when the range is empty,
/// matching `Math.floor(rand * (max - min) + min)`.
fn random_in(min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    rand::random_range(min..max)
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

/// Adds `pvp` to [`Bot`].
pub trait PvpExt {
    /// `bot.pvp` — the mineflayer-pvp namespace (`attack`, `stop`,
    /// `target`).
    fn pvp(&self) -> Pvp<'_>;
}

impl PvpExt for Bot {
    fn pvp(&self) -> Pvp<'_> {
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
    /// disconnect — each ending emits [`PvpEvent::StoppedAttacking`]
    /// exactly once. Emits [`PvpEvent::StartedAttacking`] on every
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
        let current = bot
            .state::<PvpState, _>(|s| s.target.as_ref().map(|t| t.id()))
            .flatten();
        if current == Some(entity.id()) {
            return Ok(());
        }
        let opts = bot
            .state::<PvpState, _>(|s| s.opts.clone())
            .unwrap_or_default();
        // upstream: `if (this.movements)
        // pathfinder.setMovements(this.movements)`
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
        let started = bot.state_mut::<PvpState, _>(|s| {
            s.target = Some(entity.clone());
            bot.start_op(&mut s.op)
        });
        let Some(op) = started else {
            // the client entity vanished between logged_in and here —
            // unwind the goal we just set
            bot.pathfinder().stop();
            return Err(Error::NotConnected);
        };
        tokio::spawn(drive(bot.driver(), entity.clone(), op));
        bot.emit_plugin_event(PvpEvent::StartedAttacking);
        Ok(())
    }

    /// `bot.pvp.stop()` — end the attack loop. Emits
    /// [`PvpEvent::StoppedAttacking`] and clears the follow goal when an
    /// attack was running; a no-op otherwise.
    pub fn stop(&self) {
        let stopped = self.take_target();
        let Some(target) = stopped else {
            return;
        };
        stop_follow_if_ours(self.bot, &target);
        self.bot.emit_plugin_event(PvpEvent::StoppedAttacking);
    }

    /// `bot.pvp.forceStop()` — end the attack loop and clear the
    /// pathfinder goal unconditionally (upstream's
    /// `pathfinder.setGoal(null)`): unlike [`Pvp::stop`], this also
    /// drops a goal someone else installed after `attack`. Emits
    /// [`PvpEvent::StoppedAttacking`] when an attack was running; a
    /// no-op otherwise.
    pub fn force_stop(&self) {
        if self.take_target().is_none() {
            return;
        }
        self.bot.pathfinder().stop();
        self.bot.emit_plugin_event(PvpEvent::StoppedAttacking);
    }

    /// `bot.pvp.target` — the entity the loop is driving at, `None` when
    /// idle.
    pub fn target(&self) -> Option<EntityRef> {
        self.bot
            .state::<PvpState, _>(|s| s.target.clone())
            .flatten()
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
        self.bot
            .state_mut::<PvpState, _>(|s| {
                s.op.cancel();
                s.target.take()
            })
            .flatten()
    }

    /// Read an opts field through the component (defaults pre-spawn).
    fn opts<R>(&self, f: impl FnOnce(&PvpOpts) -> R) -> R {
        let opts = self
            .bot
            .state::<PvpState, _>(|s| s.opts.clone())
            .unwrap_or_default();
        f(&opts)
    }
    /// Mutate an opts field; a silent no-op pre-spawn (there's no state
    /// to attach it to yet).
    fn set_opts(&self, f: impl FnOnce(&mut PvpOpts)) {
        self.bot.state_mut::<PvpState, _>(|s| f(&mut s.opts));
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
    let mut ticks = bot.ticks_until(&op);
    // upstream `timeToNextAttack` — attack() resets it to 0, each tick
    // decrements, and `=== -1` attempts a swing
    let mut time_to_next_attack: i64 = 0;
    let mut was_in_range = false;
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
        let (view_distance, attack_range, rate) = bot
            .state::<PvpState, _>(|s| {
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
            time_to_next_attack = rate.ticks(cooldown_ticks(&bot));
            continue;
        }
        // aim at the entity's torso, like upstream's offset(0, height, 0)
        let _ = bot
            .look_at(target_pos.up(target.2.unwrap_or(0.9)), false)
            .await;
        bot.attack(&entity, true);
        bot.emit_plugin_event(PvpEvent::AttackedTarget);
        time_to_next_attack = rate.ticks(cooldown_ticks(&bot));
    }
    report_end_if_ours(&bot, &op);
}

/// The held weapon's cooldown in ticks — the `getCooldown(heldItem)`
/// upstream's `MaxDamageOffset` adds its offset to. `0` when the bot
/// has no `Attributes` (a despawned client — the driver exits on the
/// next state check anyway).
fn cooldown_ticks(bot: &Bot) -> i64 {
    bot.client()
        .get_component::<Attributes>()
        .map(|a| get_attack_strength_delay(&a).ceil() as i64)
        .unwrap_or(0)
}

/// Report the loop's end only when the state is still ours: clear the
/// target, release the follow goal, and emit `StoppedAttacking`. A
/// `stop()`/retarget that beat the driver here already owns the outcome,
/// so a stale driver exits silently.
fn report_end_if_ours(bot: &Bot, op: &Op) {
    let ended = bot.state_mut::<PvpState, _>(|s| {
        if s.op.finish(op) {
            s.target.take()
        } else {
            None
        }
    });
    if let Some(target) = ended.flatten() {
        stop_follow_if_ours(bot, &target);
        bot.emit_plugin_event(PvpEvent::StoppedAttacking);
    }
}

#[cfg(test)]
mod tests;
