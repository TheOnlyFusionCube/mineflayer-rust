# 03. Domain modules

Baseline `7a2bc95`. Evidence for summary findings F3 to F6.

## Size

Production lines are counted up to the first `#[cfg(test)] mod tests`.

| File | Production | Total |
|---|---|---|
| `src/pathfinder/mod.rs` | 1,049 | 1,446 |
| `src/craft.rs` | 986 | 1,384 |
| `src/world/mod.rs` | 957 | 1,390 |
| `src/inventory/mod.rs` | 880 | 1,065 |
| `src/misc/mod.rs` | 826 | 1,199 |
| `src/collect.rs` | 816 | 1,061 |
| `src/chat.rs` | 762 | 1,164 |
| `src/windows/tracker.rs` | 687 | 1,134 |

Only `pathfinder/mod.rs` is over 1,000 production lines. Finding F1's
proposal P1 moves most of the reason for it into the engine.
`craft.rs` drops by about 215 lines with F4.

Functions over 100 lines (14 of 745):

| Lines | Function | Remedy |
|---|---|---|
| 297 | `pathfinder::tracking::bridge_events` | F1, P1 |
| 217 | `entity::tracker::handle_packet` | split per packet family |
| 215 | `craft::item_tag` | F4 |
| 210 | `windows::tracker::observe_event` | F1, P2, then slot helpers |
| 197 | `scoreboard::handle` | split per packet family |
| 191 | `bridge::spawn` | F2 |
| 159 | `collect::run` | extract the vacuum and deposit phases |
| 141 | `inventory::transfer::transfer` | keep; it is the upstream choreography |
| 139 | `bridge::translate` | F2 (lifecycle arms to their own fn) |
| 133 | `windows::tracker::set_content_events` | F1, P2 |
| 130 | `auto_eat::run_eat` | extract the off-hand path |
| 126 | `world::tracker::decode_packet` | split per packet family |
| 115 | `interact::dig::dig_opts` | F1, P3 (mining cancel) |
| 105 | `windows::tracker::set_slot_events` | F1, P2 |

## F3. Repeated helpers

### ECS state accessors

```sh
grep -rnE '^fn (\w+_state(_mut)?|facade(_mut)?|misc_mut)<' src   # 11
```

`tool_state`/`tool_state_mut`, `pvp_state`/`pvp_state_mut`,
`collect_state`/`collect_state_mut`, `auto_eat_state`/`auto_eat_state_mut`,
`facade`/`facade_mut`, and `misc_mut` have the same bodies. The read side
takes `ecs.read()`, gets the client entity, and maps `get::<T>()`. The
write side takes `ecs.write()`, inserts `T::default()` when missing, and
maps `get_mut::<T>()`. The differences are accidental: `facade_mut` logs
a warning when the entity is gone, and `misc_mut` returns
`Err(NotConnected)` and calls `unwrap()`.

Replacement, in `src/bot/mod.rs` next to the storage rule it serves:

```rust
impl Bot {
    /// Storage rule 1 and 2: read a component on the client entity.
    pub(crate) fn with_component<T: Component, R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let ecs = self.client.ecs.read();
        ecs.get_entity(self.client.entity).ok()?.get::<T>().map(f)
    }

    /// Storage rule 2: mutate a component, created on first use.
    pub(crate) fn with_component_mut<T: Component + Default, R>(
        &self,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let mut ecs = self.client.ecs.write();
        let mut entity = ecs.get_entity_mut(self.client.entity).ok()?;
        let mut c = entity.entry::<T>().or_default();
        Some(f(&mut c))
    }
}
```

Several callers have only a `Client`, not a `Bot` (driver tasks). Put
the pair on a small extension trait for `Client` instead if that reads
better; either way there is one copy. The lock rule comment ("`f` runs
under the ECS lock, take no domain lock") then lives in one place.

### The tick loop

`pathfinder::follow_driver` (`src/pathfinder/mod.rs:880`),
`pathfinder::dynamic_watcher` (`:976`), `pvp::drive` (`src/pvp.rs:375`),
and `auto_eat::drive` (`src/auto_eat.rs:585`) each repeat:

```rust
let mut ticks = client.get_tick_broadcaster();
let _ = shutdown.borrow_and_update();
loop {
    tokio::select! {
        r = ticks.recv() => match r {
            Ok(()) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return,
        },
        _ = shutdown.changed() => return,
    }
    // per-tick body
}
```

`Bot`'s field docs (`src/bot/mod.rs:37-53`) explain at length why the
shutdown receiver is needed (the tick sender lives in the world, so the
channel never closes while the driver holds a client). A helper such as
`Bot::next_tick(&mut TickStream) -> bool` or a `TickStream` type with
`async fn next(&mut self) -> bool` puts that rule in one place, and each
driver becomes `while ticks.next().await { ... }`. `pvp::drive` also
reports its end on shutdown; it keeps that as a line after the loop.

### Event waits

`bot::wait::await_event` returns `Option<BotEvent>`, so a caller cannot
tell a timeout from a closed stream. Six production waits are still
written by hand for that reason: `interact/place.rs:346` and `:542`,
`Bot::wait_for_chunks_to_load` (`src/bot/mod.rs:342`),
`misc/creative.rs:108`, `Inventory::await_window_open`
(`src/inventory/mod.rs:795`), and `chat::await_message_on`. Return a
`Result<BotEvent, WaitEnd>` with `Timeout` and `Closed` (and `End`) and
all six can use it.

### Slot events in the window tracker

`src/windows/tracker.rs` builds `BotEvent::WindowUpdateSlot` nine times
with the same "snapshot the old value, push the event" shape, calls
`held_sig_changed` eleven times (usually followed by a `HeldItemChanged`
push), and writes the "selected hotbar slot, else the ECS value, else
0" closure three times. Two helpers cover it:
`WindowTracker::slot_update(&mut self, window, slot, stack) -> BotEvent`
and `WindowTracker::held_changed(&mut self, client) -> BotEvent`. Do this
after F1/P2, because P2 removes several of the call sites.

## F4. The tag table

`craft::item_tag` (`src/craft.rs:492-716`) maps 207 tag names to
`mineflayer_registry::tags::items::*` statics by hand. The registry crate
is generated. The generator should emit

```rust
pub fn by_name(name: &str) -> Option<&'static RegistryTag<ItemKind>>
```

next to the statics, and `item_tag` becomes a namespace check and one
call. Until the generator is in the repository (see `04_engine_fork.md`),
the function can be added to `crates/mineflayer-registry/src/tags/items.rs`
by hand with a test that every static is reachable by its name.

## F5. Packet reducers

`entity::tracker::handle_packet` (217 lines), `scoreboard::handle` (197),
and `world::tracker::decode_packet` (126) are single `match` statements
over many packet kinds, with substantial logic in each arm. Split each
into one function per packet family (spawn/remove, movement, metadata,
equipment, effects for entities; objectives, scores, teams, boss bars for
scoreboard). `misc::handle_packet_state` already shows the target
shape: a short dispatch `match` that calls small pure functions, each
testable without a client.

## F6. Test scaffolding and connect

`Bot::new` (`src/bot/mod.rs:177`) is `pub` and `#[doc(hidden)]`. It is
called only from unit tests (34 call sites across 16 files, all after `#[cfg(test)]`). It is the
reason `Bot` has `_shutdown_tx: Option<watch::Sender<()>>` and the
"scaffold bot" paragraph in the field docs. Move it to
`crate::test_util` as `pub(crate)`; delete the field. A dependent that
needs a `Bot` without a server can use the fixture.

`BotBuilder::connect_engine` (`src/bot/connection.rs:343-365`) takes the
ECS write lock three times in a row to insert `AutoReconnectDelay`,
`MiscState`, and `InteractShared`. One `insert((a, b, c))` does the same
in one lock, and leaves no moment where only some of the components
exist. After F1/P3 the first insert goes away.

`Bot::quit` and `Bot::end` (`src/bot/mod.rs:271-281`) differ only in the
default reason. That is fine; it mirrors upstream.

## What is good and should stay

The error type is small and typed, keeps upstream messages, and has no
string catch-all. Locks are `parking_lot` with a written order. The
storage rule for per-bot state is written down once (`BotShared` docs)
and followed. Domain modules match mineflayer's docs structure, which
makes the parity table easy to check. Comments explain upstream
behavior and why the port matches or diverges, which is the right use
of comments in a port.
