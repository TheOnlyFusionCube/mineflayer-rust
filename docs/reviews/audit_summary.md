# Code quality audit: summary (third audit)

Date: 2026-10-17. Baseline: commit `ef06f74` on `main`.
Scope: the port (`src/`, 26,234 lines in 43 files, about 18,000 of them
before the test modules), its tests, docs, and tooling, the engine crates
where the port depends on their behavior, and a comparison with upstream
mineflayer 4.39.0 (`ref/mineflayer`, commit `f2b236a`).
Previous audit: commit `ef06f74`, `docs/reviews/` (second audit, all
findings closed by the second plan).
Plan: [`docs/improvement_plan.md`](../improvement_plan.md) turns these
findings into work items.

## Verdict

The port is clean at the level of single functions. The 254 unit tests
pass in 0.35 s, five production functions are over 100 lines (down from
fourteen), the second plan landed in full, and the bridge now has one
ordered observer list. A reviewer who reads one file at a time will find
little to fix.

The structure is still not acceptable as the base for a library that
should match or beat mineflayer. Two design choices cause most of the
remaining complexity, and both are visible only across files.

First, the port keeps a second model of the game beside the engine's ECS.
`BotShared` holds seven domain trackers behind nine locks, and an async
bridge task fills them by decoding every packet again after the engine
has already applied it. Because the port looks at a packet after the
engine changed the world, it has lost the information it needs: the old
block of a `blockUpdate`, the entity handle of a removed entity, and the
pre-open window state. The port then rebuilds that information with
shadow maps, retained id maps, and a copy of the engine's own window
state machine. The lock-order rule in `src/bot/mod.rs` exists only
because two lock families now hold the same facts.

Second, every long-running operation invents its own cancellation. The
pathfinder has `goal_gen`, `set_gen`, `last_failure`, a watcher cancel
flag, and a "fresh drop" skip window. Digging has `dig_seq`,
`aborted_seq`, and two grace counters. Eating has `eat_generation`.
Collect has its own generation. Auto-eat and pvp each keep `AtomicBool`
cancel flags. There are about 145 such sites in 13 files. Each one is
careful, and together they are the hardest code in the port to change.

Both problems have the same code-judo answer, and the engine already has
the hook for it. The engine runs on Bevy and exposes
`StartClientOpts::new_with_plugins` and the `ReceiveGamePacketEvent`
message. If the port's packet reducers become Bevy systems in a port-owned
plugin group, the state moves into ECS components and resources, the
domain locks and the lock-order rule disappear, and operation outcomes
become typed ECS messages that a future awaits by id. The same plugin
surface, exposed on `BotBuilder`, is the Rust answer to
`bot.loadPlugin`, which the port lists as `wontfix` today.

Upstream comparison: the port covers the API table (342 ported, 126
diverges, 47 wontfix, 11 additive), but it is behind mineflayer on
reach. Mineflayer has a plugin ecosystem, 58 examples (48 scripts and 10
projects), 46 external test files run against 28 Minecraft versions, a stable runtime, and a published
package. The port has no extension model, three examples, 14 live tests
on one version, a nightly-only toolchain that dependents must copy, and
no published crate. The port also has three correctness defects that
mineflayer does not have (F3).

The approval bar of the review skill is not met. F1 (a duplicated state
machine) and F2 (scattered special-case bookkeeping) are presumptive
blockers.

## Findings

### F1. The port keeps a second model of the game (structural)

Detail: [`01_state_ownership.md`](01_state_ownership.md).

`src/windows/tracker.rs` keeps `pending`, `early_items`, `last_closed`,
and per-window slot snapshots. The engine keeps the same state after plan
item A3: `Inventory::early_content: Option<(i32, Vec<ItemStack>)>` and
`Inventory::last_closed: Option<(i32, usize, usize)>`
(`crates/mineflayer-entity/src/inventory.rs:68-72`) have the same types
as the port's fields. Two copies of one state machine must now agree on
every packet. `src/world/tracker.rs` keeps up to 65,536 block states in
`last_block_updates` because the port reads the world after the engine
applied the update; the first update of any position still reports
`old == new`, which upstream never does. `src/entity/tracker.rs` keeps
nine `HashMap`s keyed by protocol id beside the ECS, with hand-written
`forget` and `forget_other_entities` lifecycles, and decodes metadata by
raw index (`6`, `14`, `0`) and bit mask although the engine has typed
components (`FallFlying`, `AbstractEntityShiftKeyDown`, `SleepingPos`).
The remedy is a port-owned Bevy plugin group that runs the reducers
inside the ECS, stores state as components and resources, and reads
pre-apply state through one small engine hook. `BotShared` then shrinks
to the event sender, and the lock-order rule is deleted.

### F2. Each async operation re-invents cancellation (spaghetti)

Detail: [`02_async_operations.md`](02_async_operations.md).

The port runs operations as tokio futures that poll ECS state once per
tick and race the ECS systems that change it. To tell "my operation" from
"a newer one" it needs generation counters, and to survive the gap
between an ECS change and the next poll it needs grace windows. The
pathfinder facade alone has 47 such sites (`src/pathfinder/mod.rs`),
including `follow_driver` and `dynamic_watcher`, two tokio tasks that
re-issue goals and need a `fresh_drop` skip so that they do not resurrect
a failed search. The remedy has two parts. One `Operation` primitive
built on `tokio_util::sync::CancellationToken` (the workspace already
depends on `tokio-util`) gives supersede and shutdown semantics in one
place. Completion becomes a typed ECS message with an operation id, so a
future waits for its own id instead of polling. `Goal::Follow` and
dynamic goals move into an ECS system, which deletes both watcher tasks.

### F3. Packets and errors are dropped without a signal (correctness)

Detail: [`05_engine_defects.md`](05_engine_defects.md).

A `score`, `selector`, `keybind`, `nbt`, or `object` chat component
anywhere in a packet makes `FormattedText::mc_read` fail
(`crates/mineflayer-chat/src/component.rs:569-597`). The engine then logs
the error and drops the whole packet
(`crates/mineflayer-client/src/plugins/connection.rs:101`). Vanilla's
server sends `keybind` components unchanged, so a chat line or title
that names a key is lost even on a vanilla server, and a container whose
title holds one never opens, so `open_container` times out. Server
software that does not resolve `selector` and `score` loses more. The
port has no subscriber by default, so the user sees nothing. Vanilla
renders these components as plain text, and so should the engine.
Related: `Event`, `PathEvent`, and four other enums of the owned engine
are `#[non_exhaustive]`, so the bridge needs wildcard arms that hide new
variants. Microsoft device-code login prints to stdout from library code
(`crates/mineflayer-auth/src/auth.rs:412`), with no callback.

### F4. There is no extension model (structural, ecosystem)

Detail: [`03_bridge_and_events.md`](03_bridge_and_events.md),
[`06_parity_and_ecosystem.md`](06_parity_and_ecosystem.md).

Mineflayer's value is its plugin ecosystem. The port marks
`loadPlugin`, `loadPlugins`, `hasPlugin`, and `options.plugins` as
`wontfix` ("compile-time plugin model"), but there is no compile-time
model either. The plugin analogs (`pathfinder`, `pvp`, `collect`,
`auto_eat`, `tool`) live in the core crate and depend on crate-private
helpers (`with_component`, `TickStream`, `subscribe`, `emit_event`,
`driver_handle`). Their events (`EatStart`, `CollectDone`,
`StartedAttacking`, …) are variants of the closed core `BotEvent`. A
third-party crate cannot keep per-bot state, observe packets in order, or
emit its own events. `BotBuilder` does not expose the engine's
`new_with_plugins`. The remedy is a public plugin surface (a Bevy plugin
plus a typed event channel per plugin), and then the five plugin analogs
move out of the core crate onto that public surface as proof that it is
enough.

### F5. The event API pushes boilerplate onto every user (devex)

Detail: [`03_bridge_and_events.md`](03_bridge_and_events.md).

Every example writes the same `recv()` loop with arms for `Lagged` and
`Closed`. The crate's own `await_event`/`await_map` helpers are
`pub(crate)`, so users have no `once(bot, 'event')`. A lagged receiver
loses events with no counter. Some payloads are weakly typed:
`PathReset { reason: &'static str }`, `Kicked(String, bool)`,
`PistonMove { direction: u8 }`, boss bars keyed by `String` instead of
`Uuid`. `HardcodedSound` is a variant that is never emitted. The remedy is
a public wait helper, typed `Stream` adapters, typed payloads, and
removal of the dead variant.

### F6. Local duplication and stale text (legibility)

Detail: [`04_domain_modules.md`](04_domain_modules.md).

There are two copies of the spiral block scan with early exit and sort
(`find_states`, `find_matching_unlocked` in `src/world/mod.rs`), three
eye-position helpers, three DDA raycast wrappers over `traverse_blocks`,
and two "best tool" scorers over `get_mine_progress`
(`Pathfinder::best_harvest_tool`, `Tool::best_tool_for`).
`Window::slot` clones the whole `Menu` for one read, against its own doc.
`misc::tick_diffs` takes the exclusive ECS write lock 20 times a second
to update port-owned baselines. `Window::close` still writes the engine
`Inventory`. Docs went stale after the second plan: the `windows` module
header still says the port patches `carried_item` and
`set_player_inventory`, `interact/mod.rs` names the engine `Mining`
component as the dig-target source of truth while `dig.rs` owns it, and
an orphan doc fragment from the deleted `stop_pathfinding_patched` sits
on top of `follow_driver` (`src/pathfinder/mod.rs:795`). Thirty-three
comments cite plan IDs (`N-3`, `M-4`, `U9`, …) instead of the rule.

### F7. File size comes from inline tests (decomposition)

Detail: [`04_domain_modules.md`](04_domain_modules.md).

Eleven port files are over 1,000 lines, all because of in-file test
modules (`src/bridge.rs`: 517 production lines, 771 test lines).
`src/pathfinder/mod.rs` (960 production lines) and `src/world/mod.rs`
(957) are close to the limit on production code alone. Moving each large
test module to a sibling `tests.rs` file fixes the first group. F2
removes the watcher tasks from `pathfinder/mod.rs`, and the scan merge in
F6 shortens `world/mod.rs`.

### F8. Capability gaps against upstream (product)

Detail: [`06_parity_and_ecosystem.md`](06_parity_and_ecosystem.md).

Several `diverges` rows are missing features, not naming differences. The
engine stores no block entities, so sign text and `block.blockEntity`
cannot be read. Physics settings are read-only and physics cannot be
switched off. The pathfinder never places blocks, opens doors, or honors
`allowSprinting` and `maxDropDown`; six of the ten public `Movements`
fields are accepted and ignored. Config-phase resource packs are not
visible, and the engine accepts every pack with no opt-out. Auto-respawn
cannot be switched off. `recipes_for` sees only recipes that the server
unlocked in the recipe book. Each is a feature that mineflayer users
depend on.

### F9. No in-process test drives the real engine (tests)

Detail: [`07_tests_docs_tooling.md`](07_tests_docs_tooling.md).

The port's unit tests build engine `Event`s by hand over a client with no
systems running. Nothing tests the order the port depends on: the engine
applies a packet, and then the port observes it. The engine has a
`Simulation` harness that feeds real packets through the real systems,
and the port does not use it. The live suite (14 tests) runs only on a
schedule. Typed windows have 5 to 36 percent line coverage, and the
fixture cannot produce chests, villagers, or anvils. A port-level
simulation harness would test packet-in to `BotEvent`-out on every push,
with no Java and no network. Upstream cannot do this; its tests need a
real server.

### F10. Adoption barriers (ecosystem)

Detail: [`06_parity_and_ecosystem.md`](06_parity_and_ecosystem.md).

A dependent crate must copy the pinned nightly toolchain file and the
`-Znext-solver=no` rustflag (README, "Use as a dependency"). The engine
needs only seven feature gates, and each is local (two trait aliases,
three `default fn` sites, one `#[backtrace]`, one const-expression site,
one struct-update site), so stable Rust is within reach. Nothing is
published and there is no public repository, so there is no docs.rs page.
There are three ported examples against upstream's 48 example
scripts and 10 example projects.

## Remediation sequence

1. Engine defects (F3): chat components degrade to text, decode errors
   surface as a warning event, owned enums lose `#[non_exhaustive]`, auth
   takes a code callback. Small changes with a user-visible result.
2. The port simulation harness (F9). It is the safety net for steps 3
   and 4, which move code between layers.
3. State ownership (F1): the port plugin group and the pre-apply hook,
   then one domain at a time (world, windows, entities, scoreboard and
   chat, misc). Delete the domain locks and the lock-order rule at the
   end.
4. The operation primitive and completion messages (F2). Follow and
   dynamic goals move into the ECS.
5. The public plugin surface and the event DX (F4, F5). Move the five
   plugin analogs out of the core crate.
6. Local cleanups and file splits (F6, F7).
7. Capability parity (F8), in the order users need it.
8. Adoption (F10): stable toolchain, a publish decision, examples, docs.

Steps 1 and 2 can start at once and run in parallel. The parts of step
6 that touch code which steps 3 and 4 do not rewrite (the block scan,
the eye and raycast helpers, the stale text, the test file splits) can
run beside them. The operation primitive of step 4 can start beside
step 3, but the rewrites of `goto`, `dig`, and the eat paths wait for
step 3, because they change the same files. Step 4 must finish before
step 5. Each step keeps public
behavior unless its plan item says otherwise, and each is checked by the
gate, the simulation harness, and the live suite.
