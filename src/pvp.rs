//! The mineflayer-pvp plugin surface: `bot.pvp()` — a background
//! follow+attack loop.
//!
//! `attack(entity)` installs a dynamic [`Goal::Follow`] (the same
//! mechanism `pvp`'s upstream `GoalFollow(target, reach)` uses) and
//! spawns a per-tick driver: while the target is loaded and alive, the
//! bot looks at it and swings whenever it's within reach and the attack
//! cooldown is ready. `stop()` — or the target dying/despawning — ends
//! the loop, clears the follow goal, and emits
//! [`BotEvent::StoppedAttacking`] exactly once (whoever ends the loop
//! owns the report).

use std::sync::{
    Arc,
    atomic::{self, AtomicBool},
};

use azalea::{
    EntityRef,
    ecs::{self as bevy_ecs, prelude::Component},
    entity::{Position, metadata::Health},
};

use crate::{Bot, BotEvent, Error, Result, pathfinder::Goal};

/// Vanilla's entity-interaction range — the gate upstream pvp applies
/// before swinging.
const ATTACK_RANGE: f64 = 3.;
/// How close the follow goal brings the bot — inside [`ATTACK_RANGE`]
/// with slack for the target's drift between block-position re-paths.
const FOLLOW_RANGE: f32 = 2.;

/// Facade state azalea has no slot for: the pvp target plus the cancel
/// flag for its driver task — the same shape [`crate::pathfinder`] uses
/// for its watcher (`PathfinderFacade.watcher_cancel`).
#[derive(Component, Default)]
struct PvpState {
    /// The entity `attack` is driving at; `bot.pvp.target`.
    target: Option<EntityRef>,
    /// Set to retire the running driver; replaced on every `attack`.
    cancel: Option<Arc<AtomicBool>>,
}

/// Read from the [`PvpState`] component. `None` when it or the entity
/// doesn't exist.
fn pvp_state<R>(client: &azalea::Client, f: impl FnOnce(&PvpState) -> R) -> Option<R> {
    let ecs = client.ecs.read();
    let entity = ecs.get_entity(client.entity).ok()?;
    entity.get::<PvpState>().map(f)
}

/// Run `f` against [`PvpState`], inserting a default one first. `None`
/// when the client entity is gone.
fn pvp_state_mut<R>(client: &azalea::Client, f: impl FnOnce(&mut PvpState) -> R) -> Option<R> {
    let mut ecs = client.ecs.write();
    let mut entity = ecs.get_entity_mut(client.entity).ok()?;
    if entity.get::<PvpState>().is_none() {
        entity.insert(PvpState::default());
    }
    entity.get_mut::<PvpState>().map(|mut c| f(&mut c))
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
    /// it stays loaded and alive, swinging on every in-range tick the
    /// cooldown allows. A second `attack` retargets the loop; the loop
    /// ends on [`Pvp::stop`], the target's death/despawn, or disconnect —
    /// each ending emits [`BotEvent::StoppedAttacking`] exactly once.
    ///
    /// # Errors
    ///
    /// [`Error::NotConnected`] before the bot has spawned.
    pub fn attack(&self, entity: &EntityRef) -> Result<()> {
        let bot = self.bot;
        if !bot.logged_in() {
            return Err(Error::NotConnected);
        }
        // set the goal first — if it fails nothing was recorded
        bot.pathfinder().set_goal(
            Goal::Follow {
                entity: entity.clone(),
                range: FOLLOW_RANGE,
            },
            true,
        )?;
        let cancel = Arc::new(AtomicBool::new(false));
        let recorded = pvp_state_mut(&bot.client, |s| {
            // a retarget retires the previous driver — it sees the flag or
            // the replaced cancel and exits silently
            if let Some(old) = s.cancel.replace(cancel.clone()) {
                old.store(true, atomic::Ordering::Relaxed);
            }
            s.target = Some(entity.clone());
        });
        if recorded.is_none() {
            // the client entity vanished between logged_in and here —
            // unwind the goal we just set
            bot.pathfinder().stop();
            return Err(Error::NotConnected);
        }
        tokio::spawn(drive(bot.clone(), entity.clone(), cancel));
        Ok(())
    }

    /// `bot.pvp.stop()` — end the attack loop. Emits
    /// [`BotEvent::StoppedAttacking`] and clears the follow goal when an
    /// attack was running; a no-op otherwise.
    pub fn stop(&self) {
        let stopped = pvp_state_mut(&self.bot.client, |s| {
            if let Some(cancel) = s.cancel.take() {
                cancel.store(true, atomic::Ordering::Relaxed);
            }
            s.target.take()
        })
        .flatten();
        let Some(target) = stopped else {
            return;
        };
        stop_follow_if_ours(self.bot, &target);
        self.bot.emit_event(BotEvent::StoppedAttacking);
    }

    /// `bot.pvp.target` — the entity the loop is driving at, `None` when
    /// idle.
    pub fn target(&self) -> Option<EntityRef> {
        pvp_state(&self.bot.client, |s| s.target.clone()).flatten()
    }
}

/// The attack loop: one iteration per game tick. Owns the
/// `StoppedAttacking` report only when it ends on its own (target gone —
/// `stop()` and `attack`-retargets already reported or made the loop
/// stale); a stale or cancelled driver exits silently so the event can
/// never double-fire.
async fn drive(bot: Bot, entity: EntityRef, cancel: Arc<AtomicBool>) {
    let mut ticks = bot.client.get_tick_broadcaster();
    loop {
        match ticks.recv().await {
            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
        if cancel.load(atomic::Ordering::Relaxed) {
            return;
        }
        // copy what we need out and drop the component guards before any
        // other ECS access — a read guard held across another lock
        // deadlocks behind a queued writer
        let target = {
            let pos = entity.get_component::<Position>().map(|p| **p);
            let health = entity.get_component::<Health>().map(|h| h.0);
            (pos, health)
        };
        let gone = match target {
            // despawned, or dead (upstream's `isValid` check ends the same
            // way — a dying entity keeps its position through the death
            // animation, so health gates it)
            (Some(_), Some(h)) if h <= 0. => true,
            (None, _) => true,
            _ => false,
        };
        if gone {
            // report only if the state is still ours: a stop()/retarget
            // that beat us here already owns the outcome
            let ended = pvp_state_mut(&bot.client, |s| {
                let mine = s.cancel.as_ref().is_some_and(|c| Arc::ptr_eq(c, &cancel));
                if mine {
                    s.cancel = None;
                    s.target.take()
                } else {
                    None
                }
            });
            if let Some(target) = ended.flatten() {
                stop_follow_if_ours(&bot, &target);
                bot.emit_event(BotEvent::StoppedAttacking);
            }
            return;
        }
        let Some(target_pos) = target.0 else {
            return;
        };
        // aim at the entity's torso, like upstream's offset(0, height, 0)
        let _ = bot.look_at(target_pos.up(0.9), false).await;
        if bot.position().distance_to(target_pos) <= ATTACK_RANGE
            && !bot.client.has_attack_cooldown()
        {
            bot.attack(&entity, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use azalea::{Vec3, registry::identifier::Identifier, world::WorldName};

    use super::*;
    use crate::test_util::{despawned_client, live_entity_client};

    /// A bot whose client entity is alive and carries a `WorldName` —
    /// enough for `Bot::logged_in` to pass. No systems run on this ECS,
    /// so the spawned driver/follow tasks die immediately (no
    /// `TickBroadcast` resource) — the synchronous state transitions are
    /// what's under test here.
    fn spawned_bot() -> Bot {
        Bot::new(live_entity_client((
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

    /// Drain every pending event; report whether a `StoppedAttacking`
    /// was among them.
    fn saw_stopped_attacking(rx: &mut tokio::sync::broadcast::Receiver<BotEvent>) -> bool {
        let mut saw = false;
        while let Ok(event) = rx.try_recv() {
            saw |= matches!(event, BotEvent::StoppedAttacking);
        }
        saw
    }

    #[tokio::test]
    async fn attack_then_stop_transitions_and_reports_once() {
        let bot = spawned_bot();
        let mut rx = bot.events();
        let target = spawn_target(&bot, 5., 0.);

        // start → engaged: the loop records its target
        bot.pvp().attack(&target).unwrap();
        assert_eq!(bot.pvp().target().map(|t| t.id()), Some(target.id()));

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
        // a retarget never stopped the loop — no StoppedAttacking
        assert!(!saw_stopped_attacking(&mut rx));

        bot.pvp().stop();
    }

    #[tokio::test]
    async fn attack_before_spawn_is_not_connected() {
        let client = despawned_client();
        let bot = Bot::new(client.clone());
        let target = client.entity_ref_for(client.entity);
        assert!(matches!(
            bot.pvp().attack(&target),
            Err(Error::NotConnected)
        ));
        assert!(bot.pvp().target().is_none());
    }
}
