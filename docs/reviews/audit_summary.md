# Code quality audit: summary

Date: 2026-10-16. Baseline: commit `7a2bc95` on `main` (the engine in
`crates/`, the log-subscriber fix applied).
Scope: the port (`src/`, about 17,000 production lines in 40 files), its
tests, docs, and tooling, and the boundary between the port and the
in-tree engine. The engine crates themselves (about 123,000 lines) were
audited only where the port touches them; see
[`04_engine_fork.md`](04_engine_fork.md).
Plan: [`docs/improvement_plan.md`](../improvement_plan.md) turns these
findings into work items.

## Verdict

The port works. The gate passes (fmt, clippy, 260 port tests and 9
doctests, 382 engine tests, docs) and the live suite passes 14 of 14 on a
fresh seeded world. The earlier plan was carried out completely: typed
errors, `parking_lot` locks with a written lock order, one shared event
wait, and split domain modules.

The structure is still not acceptable as a long-term base. Most of the
remaining complexity has one cause, and that cause disappeared today.
The port was written against a third-party engine that it could not
change, so every engine gap was papered over from the outside. The port
diffs ECS components every tick to recover events the engine never
emits. It writes into the engine's `Inventory` component to correct
packets the engine handles wrongly. It mirrors the engine's private app
bootstrap to slip in one plugin. It disables the engine's auto-reconnect
with a sentinel duration, and it carries a "stop pathfinding" workaround
for an engine bug. Each workaround is careful and well commented, and
together they make up a large share of the hardest code in the
repository.

The engine is now in this repository. Every one of those workarounds can
move to the layer that owns the behavior, and most of them can then be
deleted. That is the main code-judo move of this audit, and the plan is
built around it.

Second-order problems remain inside the port: six copies of the same ECS
state accessor pair, four copies of the same tick-driver loop, a bridge
whose observers have seven different shapes, a 207-arm hand-written tag
table, fourteen functions over 100 lines, and an exclusive ECS lock taken
for every clientbound packet. None of these is a defect today, but each
makes the next change harder than it needs to be.

## Findings

### F1. Engine gaps are patched from outside the engine (structural)

Detail: [`01_engine_boundary.md`](01_engine_boundary.md).

The worst case is `src/pathfinder/tracking.rs`. The engine reports path
results through ECS messages (`PathFoundEvent`, `StopPathfindingEvent`)
that never reach the client `Event` stream, so the port snapshots the
`Pathfinder` and `ExecutingPath` components on every tick and infers what
happened. The engine runs its goto chain once per frame (about 17 ms)
while the diff runs per game tick (50 ms), so a fast search can start and
fail inside one gap. The port then needs a "steady-state wedge" heuristic
with a 100 ms grace window, a cumulative search budget, and a latch
against repeated timeout reports. `bridge_events` is 297 lines, the
longest function in the port, and nine unit tests guard the heuristic.
If the engine emitted path results as events, the heuristic, the budget
tracking, and most of that function would go away.

`src/windows/tracker.rs` has the same problem with inventory. The engine
ignores `carried_item` on `container_set_content`, drops
`set_player_inventory`, skips non-hotbar `set_slot` writes while a
container is open, drops slot data that arrives before `open_screen`, and
adopts `state_id` from only some packets. The bridge corrects all of this
by writing into the engine's `Inventory` component in seven places, each
under a domain lock with a lock-order comment. These are engine bugs that
desync the cursor and send clicks with a stale state id. The fix belongs
in the engine's inventory packet handlers, where it also stops the domain
lock from ever needing the ECS write lock.

Smaller cases follow the same pattern. `src/client_app.rs` copies the
engine's private `StartClientOpts::new` plugin list because the engine
has no plugin hook, and a "re-check when the engine changes" contract
guards the copy. `connect` inserts `AutoReconnectDelay(Duration::MAX)` to
switch off a plugin the port never wants. `stop_pathfinding_patched`
works around an engine stop handler that leaves `is_calculating` set. The
dig-abort reconciler and its generation bookkeeping exist because engine
mining has no cancel. The remedy is the same each time: fix or extend the
engine, then delete the port-side copy.

### F2. The bridge has no single observer contract (structural)

Detail: [`02_bridge_and_events.md`](02_bridge_and_events.md).

`bridge::spawn` is a 191-line loop that calls seven domain observers with
seven different signatures: `movement::bridge_events`,
`pathfinder::bridge_events`, `entity::observe_event`,
`windows::observe_event`, `window_types::observe_event`,
`craft::observe_event` (which returns nothing), and
`world::observe_event`. It then sends each result vector in its own
`for` loop. The misc domain is different again: `misc::after_event` runs
once per translated `BotEvent`, decodes `BotEvent::Packet` instead of the
engine event, and also routes all scoreboard packets, although the
scoreboard state lives in `BotShared` and has nothing to do with misc.
One contract (each domain takes the context and the engine event and
returns its events, called in a fixed list) would turn the loop body into
a short ordered list and make the event order visible in one place.

`misc::handle_packet` takes the exclusive ECS write lock for every
non-scoreboard clientbound packet, about 5,000 times a second at the
measured peak (8,751 events/s, about 60% raw packets), although only `ResourcePackPush` changes misc state. The
write lock competes with the engine's own systems. Matching the packet
kind first removes almost all of these lock acquisitions.

### F3. The same helpers are written many times (spaghetti)

Detail: [`03_domain_modules.md`](03_domain_modules.md).

Six modules (`tool`, `pvp`, `collect`, `auto_eat`, `pathfinder`, `misc`)
each define a pair of functions that read an ECS component on the client
entity, or insert a default one and mutate it. The bodies are the same
apart from the type name; there are eleven of these functions. One
generic pair over `T: Component + Default` replaces all of them.

Four background drivers (`pathfinder::follow_driver`,
`pathfinder::dynamic_watcher`, `pvp::drive`, `auto_eat::drive`) open with
the same block: take the tick broadcaster, mark the shutdown receiver
seen, then `select!` on ticks (tolerating `Lagged`) and shutdown. The
`goto` and `dig` waits repeat the first half of it. A single tick-loop helper would carry
the shutdown rule that `Bot`'s field docs spend three paragraphs
explaining.

Inside `windows/tracker.rs`, nine pushes of `WindowUpdateSlot` follow
the same "snapshot, then push" shape, `held_sig_changed` plus a
`HeldItemChanged` push appears eleven times, and the selected-slot
closure is written three times. Six production receive loops are still
written by hand next to the shared `wait::await_event` (two in
`interact/place.rs`, and one each in `wait_for_chunks_to_load`,
`misc/creative.rs`, `await_window_open`, and `chat::await_message_on`).
They exist because `await_event` returns `Option`, so a caller cannot
tell a timeout from a closed stream. A result type with those two cases
would let all six use the shared wait.

### F4. A data table is written by hand (missed simplification)

`craft::item_tag` (`src/craft.rs:492`) is a 215-line `match` with 207
arms that maps tag names to the static tags in the registry crate. The
registry crate is generated code that we now own. Its generator can emit
a name lookup next to the statics, and the port function becomes one
line. The current table also goes stale silently when a Minecraft
version adds a tag.

### F5. Long functions in busy flows (legibility)

Fourteen production functions are over 100 lines. The top five are
`pathfinder::tracking::bridge_events` (297), `entity::tracker::handle_packet`
(217), `craft::item_tag` (215), `windows::tracker::observe_event` (210),
and `scoreboard::handle` (197). F1 and F4 remove three of them outright.
`entity::tracker::handle_packet` and `scoreboard::handle` are packet
reducers that should split into one function per packet family, as
`misc::handle_packet_state` already does.

### F6. Test-only scaffolding lives in the public type (boundary)

`Bot::new` is `pub` with `#[doc(hidden)]`, and every caller is a unit
test. It is the only reason `Bot` carries `_shutdown_tx: Option<..>`, and
the "scaffold bot" mode adds a paragraph to the field docs. Moving it to
`test_util` deletes the field and the mode. In the same file,
`BotBuilder::connect_engine` inserts three components with three separate
`ecs.write()` calls where one bundle insert would do the same thing
atomically.

### F7. The engine fork needs ownership rules (lifecycle)

Detail: [`04_engine_fork.md`](04_engine_fork.md).

The fork compiles, passes its own tests, and is clippy and fmt clean
under the pinned toolchain. Three ownership questions are open. The
generator that produced the registry, block, and packet code was not
copied, so the next Minecraft version depends on the upstream generator.
About 1,300 lines of swarm code are compiled only because the copied
bootstrap adds `DefaultSwarmPlugins`, and auto-reconnect is compiled only
to be switched off. All 22 crate names are free on crates.io today, but
publishing the port would mean publishing 21 engine crates first; if
that is not planned, the engine crates should be `publish = false`.

### F8. Tests, docs, and tooling (maintenance)

Detail: [`05_tests_docs_tooling.md`](05_tests_docs_tooling.md).

The live suite runs only on a developer machine; CI runs the gate
without it. `docs/surveys/engine-features.md` describes an engine
revision older than the base commit (it mentions types that do not
exist at `ddef371`) and now reads as a historical note. `scripts/argon-unit.py`
and its test are personal model-delegation tooling, unrelated to the
crate. `target/` is 31 GB again because the gate now also builds the
engine crates with a second feature set.

## Remediation sequence

1. Engine events for pathfinder results (F1). This deletes the most
   code and the only heuristic in the port.
2. Engine inventory handler fixes (F1). This removes seven ECS writes
   from the bridge and the lock-order exceptions that go with them.
3. Engine plugin hook and opt-out (F1). This deletes `client_app`'s copy
   of the bootstrap, the auto-reconnect sentinel, and the swarm plugins.
4. One observer contract in the bridge, with scoreboard routed directly
   and the misc lock fixed (F2).
5. Shared helpers: the state accessor pair, the tick loop, the slot
   event helpers (F3).
6. Generated tag lookup (F4) and reducer splits (F5).
7. Test scaffolding and atomic connect (F6).
8. Fork ownership: generator, dead plugins, crate names (F7).
9. Live suite in CI, doc and tooling cleanup (F8).

Each step keeps behavior and is checked by the gate and the live suite.
Steps 1 to 3 change the engine and should each land with an engine-level
test that shows the old wrong behavior.
