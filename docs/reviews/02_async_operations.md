# 02. Async operations: one cancellation protocol per operation

Finding F2 in [`audit_summary.md`](audit_summary.md). Baseline `ef06f74`.

## The shape every operation shares

`goto`, `dig`, `consume`, `auto_eat().eat`, `collect`, `pvp().attack`,
the follow and dynamic goal watchers, and `creative().set_inventory_slot`
all have the same structure:

1. The `Bot` method writes an ECS message or a packet.
2. A tokio future then polls ECS components once per game tick (through
   `TickStream` or `client.get_tick_broadcaster()`) and also listens on the
   `BotEvent` broadcast.
3. A newer call of the same operation, an explicit stop, or the bridge's
   shutdown must end the older future. Because the future and the ECS
   systems run at different times, the future cannot tell its own outcome
   from a newer one by reading state, so it compares a generation number.
4. Because an ECS change and the next poll can be up to one tick apart,
   the future needs grace windows and skip rules.

Each operation implements steps 3 and 4 by hand.

## Measurement

Command: an awk count, per file and before `#[cfg(test)]`, of lines that
match `goal_gen|set_gen|dig_seq|aborted_seq|eat_generation|generation|AtomicBool|watcher_cancel|eat_cancel|oneshot::Sender<()>|in_flight|TickStream::new|get_tick_broadcaster`.

| File | Sites |
|---|---|
| `src/pathfinder/mod.rs` | 47 |
| `src/collect.rs` | 25 |
| `src/auto_eat.rs` | 21 |
| `src/interact/dig.rs` | 19 |
| `src/pvp.rs` | 7 |
| `src/pathfinder/tracking.rs` | 5 |
| `src/misc/creative.rs` | 5 |
| `src/inventory/mod.rs` | 5 |
| `src/windows/mod.rs` | 4 |
| `src/misc/mod.rs` | 3 |
| `src/windows/tracker.rs` | 2 |
| `src/pathfinder/movements.rs` | 1 |
| `src/bot/mod.rs` | 1 |
| **Total** | **145** |

The count is approximate. The pattern also matches doc comments and the
`TickStream::new` calls, so it measures how widespread the protocols are,
not an exact number of statements.

## The protocols, one by one

**Pathfinder** (`src/pathfinder/mod.rs`, `tracking.rs`).
`PathfinderFacade` carries `goal_gen` (bumped by `set_goal` and
`set_movements`), `set_gen` (bumped by `set_goal` only, unless the new
goal is `same_goal` as the old), `last_failure`, and `watcher_cancel:
Option<Arc<AtomicBool>>`. `goto` (97 lines) selects over the event
stream, the shutdown watch, and the tick stream, and on each tick reads
the facade to decide between `GoalChanged`, a recorded failure, success,
and `PathStopped`. `set_goal_inner` writes the facade before the ECS so
that a bridge tick cannot see a "clear edge" with no owner (comment
`N-3`). `PathTracker` in the bridge keeps a cumulative search budget
(`search_spent`, `saw_success`, `timeout_reported`) per `goal_gen`.
`follow_driver` (80 lines) and `dynamic_watcher` are tokio tasks that
re-issue goals each tick. Both need `pathfinder_live` and a `fresh_drop`
rule: if the engine dropped the goal since the last tick, skip one tick so
that the bridge can record the failure first, or else the watcher
re-installs a goal that nobody owns (comment `M-4`).

**Dig** (`src/interact/dig.rs`). `InteractShared` carries `dig_seq`,
`aborted_seq`, and `dig_target` tagged with its seq. `dig_wait` polls
`Mining`, `MiningQueued`, and the abort seq each tick, with
`DIG_START_GRACE_TICKS` before the engine picks the dig up and the same
grace again after `Mining` disappears. `cancel_engine_dig` and
`settle_dig` each take the ECS write lock to check that the seq is still
theirs.

**Consume and auto-eat** (`src/inventory/mod.rs:653`, `src/auto_eat.rs`).
`WindowTracker.eat_generation` and `eat_notify: Arc<Notify>` live in the
window tracker, although they are not window state. `consume` bumps the
generation and wakes the old waiter so it fails its generation check.
`AutoEatState` adds `eat_cancel` and `driver`, two more `AtomicBool`
flags.

**Collect** (`src/collect.rs`). `CollectState.generation` is a fourth
generation counter; `stale()` reads it through the ECS on every loop
step.

**Pvp** (`src/pvp.rs`). `PvpState.cancel: Option<Arc<AtomicBool>>` and
`report_end_if_ours`, which compares the cancel flag by pointer to decide
who reports `StoppedAttacking`.

**Shutdown**. `Bot._alive`, `Bot.shutdown`, `driver_handle`, and
`TickStream` exist so that a driver task stops when the bridge exits,
because the tick broadcast never closes while a `Client` exists
(`src/bot/mod.rs:118-155`). Three paragraphs of field docs explain it.

## What is wrong with this

Each protocol is correct as far as the tests show. The problem is that
there are six of them, each with its own names, its own grace rules, and
its own comment trail of past races. A new operation (for example a
`place_block` that waits for its own confirmation, or a pathfinder that
places scaffolding) has no shared primitive to use and will add a
seventh. Changes to tick timing in the engine can break any of them in
a different way.

## Proposal

### One operation primitive

```rust
// src/op.rs (new)
use tokio_util::sync::CancellationToken;

/// One in-flight operation of a kind (dig, goto, eat, collect, attack).
/// Starting a new operation of the same kind cancels the old one.
pub(crate) struct OpSlot { current: Option<CancellationToken> }

impl OpSlot {
    pub(crate) fn start(&mut self, bot_token: &CancellationToken) -> CancellationToken {
        if let Some(old) = self.current.take() { old.cancel(); }
        let token = bot_token.child_token();
        self.current = Some(token.clone());
        token
    }
    pub(crate) fn cancel(&mut self) {
        if let Some(t) = self.current.take() { t.cancel(); }
    }
}
```

`Bot` holds one root `CancellationToken` that the bridge cancels on exit.
Every driver task and every wait selects on `token.cancelled()`. This
replaces the `shutdown` watch, `TickStream`'s shutdown arm,
`driver_handle`'s purpose, the `AtomicBool` cancel flags, and the
generation numbers that only mean "superseded". `tokio-util` is already a
workspace dependency (`Cargo.toml:115`), so the port adds a dependency
line, not a new crate to the lock file.

### Completion messages with an id

The engine's pathfinder already counts searches (`goto_id`,
`crates/mineflayer-engine/src/pathfinder/mod.rs:121`). Let
`start_goto_with_opts` return that id, and let `PathEvent` carry it. Then
`goto` waits for `PathEvent { id: mine, .. }` and needs no `set_gen`,
`last_failure`, or tick poll. The same rule applies to mining: the
engine's `StartMiningBlockEvent` gets an id, and a `MiningFinished { id,
outcome }` message replaces `dig_wait`'s poll and both grace counters.
Where the outcome comes from a port plugin (eat finish on entity status
9, a slot confirmation), the port system emits the message, so no engine
change is needed.

### Follow and dynamic goals inside the ECS

`Goal::Follow` becomes a component, `FollowTarget { entity, range }`,
read by a port system each tick that re-issues the radius goal when the
target's block position changes. A dynamic goal becomes a
`DynamicGoal(Arc<dyn Goal>)` component, read by a system that re-issues
the goal when it stops holding. Both systems run in the schedule after
the engine's pathfinder systems, so they see the engine's goal drop in
the same pass, and the `fresh_drop` window and `pathfinder_live` are not
needed. `follow_driver`, `dynamic_watcher`, and `cancel_watcher` are
deleted.

### The cumulative search budget

Upstream's `thinkTimeout` bounds one goal end to end. The engine gives
each repath a fresh `max_timeout`. Today the bridge adds up `elapsed`
across `PathFound` events (`PathTracker`). With goto ids, the budget is
a field of the goto, and the engine (or a port system that owns the goto
record) ends it with `Timeout`. `PathTracker` is deleted.

## Expected result

| Removed | Lines (approx.) |
|---|---|
| `follow_driver`, `dynamic_watcher`, `pathfinder_live`, `cancel_watcher` | 160 |
| `PathTracker` and the budget arms of `bridge_events` | 110 |
| the tick-poll half of `goto` | 60 |
| `dig_wait` grace logic, `cancel_engine_dig`, the seq fields | 120 |
| `eat_generation`, `eat_notify`, and the auto-eat cancel flags | 50 |
| `TickStream`, `_alive`/`driver_handle` shutdown plumbing | 50 |

The numbers are estimates from the current function lengths. The plan
item records the measured result.

## Risks

Upstream semantics depend on exact timing in a few places (a `goto` on
an already satisfied goal emits `goal_updated` then `goal_reached`; a
timed-out search leaves upstream's `stateGoal` installed). Each such rule
has a unit test today. Keep those tests and port them to the simulation
harness before the rewrite, so that the new primitive is checked against
the same cases.
