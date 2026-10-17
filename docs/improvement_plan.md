# mineflayer-rs: holistic improvement plan (second plan)

Language: ASD-STE100 (Simplified Technical English).
Baseline: commit `7a2bc95` on `main`.
Date: 2026-10-16.

## 0. Information about this plan

### 0.1 Purpose

This plan gives the work that makes the port simpler, safer, and easier
to maintain. The plan does not add new mineflayer features. The binding
specification is still `docs/api-parity.md`.

The first plan is complete. It is in git at commit `775ba66`
(`docs/improvement_plan.md`).

### 0.2 Sources of the findings

The findings come from the code quality audit in `docs/reviews/`:

1. `audit_summary.md` gives the verdict and the findings F1 to F8.
2. `01_engine_boundary.md` to `05_tests_docs_tooling.md` give the
   evidence, the measurements, and the worked proposals.

Each work item names its finding and its detail file. Read the detail
file before you start the work item.

### 0.3 The main idea of this plan

Before commit `ed84001`, the engine was a third-party dependency. The
port could not change it. Thus the port corrected engine gaps from the
outside. These corrections are the most difficult code in the port.

Now the engine is in `crates/`, and the port owns it. Phase A moves each
correction into the engine crate that owns the behavior. Then Phase A
removes the correction from the port. The other phases make the port
code smaller and more regular.

### 0.4 Priority codes

| Code | Meaning |
|---|---|
| P1 | The item removes a heuristic, a write into engine state, or a copy of engine code. Do this work first. |
| P2 | The item removes repeated code or makes a boundary clear. Do this work after P1. |
| P3 | The item is a quality improvement. Do this work when time is available. |

Effort is in half-days (HD). One HD is approximately four hours.

### 0.5 General warnings and cautions

**WARNING:** Do not change the order of the domain observers in
`src/bridge.rs` before you write the order test in item F3. Users can
depend on the order of events. A changed order can cause incorrect bot
behavior without a test failure.

**CAUTION:** Do not change the toolchain pin in `rust-toolchain.toml`.
Do not remove `-Znext-solver=no` from `.cargo/config.toml`. Newer
nightly compilers stop with an internal compiler error on the engine
crates.

**CAUTION:** Do not rename game data that contains the word "azalea"
(blocks, sound events, tags, language strings). It is Minecraft data. It
is not the name of the upstream project.

**CAUTION:** Each change to `crates/` makes the next upstream merge more
difficult. Keep each engine change small. Put each engine change in its
own commit. Record each engine change in the list in item E2.

### 0.6 Rules for each work item

1. Do one work item in one commit series.
2. Write the regression test before you change the production code.
3. Run `scripts/verify.sh`. All eight steps must pass.
4. If the item changes behavior on the wire, reset the fixture and run
   the live suite:
   `bash scripts/live-server.sh reset`, then
   `cargo test --features live --no-fail-fast -- --ignored --test-threads=1`.
5. If the item changes a public API, update the related rows in
   `docs/api-parity.md` and `CHANGELOG.md` in the same commit.
6. If the item changes the engine, add an engine-level test. Use the
   simulation harness in `crates/mineflayer-client/src/test_utils/`.

## 1. Baseline state

| Item | Value |
|---|---|
| Port production source | approximately 17,000 lines in 40 files |
| Engine source (`crates/`) | 122,646 lines in 21 crates |
| Largest port production files | `pathfinder/mod.rs` (1,049), `craft.rs` (986), `world/mod.rs` (957) |
| Port functions longer than 100 lines | 14 of 745 |
| Port unit tests | 260 tests and 9 doctests |
| Engine tests | 382 |
| Live tests | 14 tests in one binary; 14 of 14 pass on a reset world |
| ECS writes into the engine `Inventory` from `windows/tracker.rs` | 7 |
| Copies of the ECS state accessor pair | 11 functions in 6 modules |
| Hand-written event waits beside `await_event` | 6 |
| `BotEvent` variants | 123 |
| Gate steps (`scripts/verify.sh`) | 8 |
| Size of `target/` | 31 GB |

## 2. Summary of the work items

| ID | Title | Finding | Priority | Effort |
|---|---|---|---|---|
| A1 | Pathfinder results as client events | F1 | P1 | 3 HD |
| A2 | Correct the engine stop handler | F1 | P1 | 0.5 HD |
| A3 | Correct the engine inventory packet handlers | F1 | P1 | 3 HD |
| A4 | A plugin hook and plugin opt-outs in the engine bootstrap | F1 | P1 | 1.5 HD |
| A5 | A cancel for queued mining | F1 | P1 | 1.5 HD |
| A6 | Engine events for the remaining re-decodes | F1 | P3 | 3 HD |
| B1 | One observer contract in the bridge | F2 | P2 | 2 HD |
| B2 | The scoreboard is its own observer | F2 | P2 | 0.5 HD |
| B3 | The misc observer takes the ECS lock only when it writes | F2 | P2 | 0.5 HD |
| B4 | The bridge exit is its own function | F2 | P3 | 0.5 HD |
| C1 | One ECS state accessor pair | F3 | P2 | 1 HD |
| C2 | One tick stream for the drivers | F3 | P2 | 1 HD |
| C3 | One event wait with a typed result | F3 | P2 | 1 HD |
| C4 | Slot event helpers in the window tracker | F3 | P3 | 0.5 HD |
| C5 | Remove the pass-through methods on `ChatShared` | F3 | P3 | 0.5 HD |
| D1 | A generated tag lookup | F4 | P2 | 1 HD |
| D2 | Split the packet reducers | F5 | P3 | 2 HD |
| D3 | Shorten the remaining long functions | F5 | P3 | 1.5 HD |
| D4 | Move `Bot::new` to the test utilities | F6 | P2 | 0.5 HD |
| D5 | One atomic component insert in `connect` | F6 | P3 | 0.5 HD |
| E1 | A decision about the engine code generator | F7 | P1 | 1 HD |
| E2 | Keep the list of local engine changes current | F7 | P1 | 0.5 HD |
| E3 | A publication policy for the engine crates | F7 | P3 | 0.5 HD |
| E4 | Remove engine modules that the port does not use | F7 | P3 | 1.5 HD |
| E5 | Engine build profile and documentation gate | F7 | P3 | 0.5 HD |
| F1 | The live suite in continuous integration | F8 | P2 | 1.5 HD |
| F2 | Engine tests for the Phase A items | F8 | P1 | in each A item |
| F3 | A test for the event emission order | F2, F8 | P1 | 0.5 HD |
| G1 | Move the historical documents | F8 | P3 | 0.5 HD |
| G2 | Remove or document the model-delegation scripts | F8 | P3 | 0.5 HD |
| G3 | Build directory size | F8 | P3 | 0.5 HD |

Total effort: approximately 33 HD.

## 3. Phase A: the engine boundary

Detail file: `docs/reviews/01_engine_boundary.md`.

### A1. Pathfinder results as client events (P1)

**Problem.** The engine sends path results as ECS messages
(`PathFoundEvent`, `StopPathfindingEvent`). These messages do not go into
the client `Event` stream. Thus the port compares component snapshots on
each tick (`src/pathfinder/tracking.rs:74`, 297 lines). A fast search can
start and fail between two ticks. Thus the port also uses a "wedge"
heuristic with a grace time of 100 ms (`WEDGE_GRACE`) and a cumulative
search budget.

**Procedure.**

1. Write an engine simulation test. Start a goto to a goal that the bot
   cannot reach. Make sure that the test fails because no path event
   arrives on the event channel.
2. In `crates/mineflayer-engine/src/pathfinder/`, add the path events to
   the client `Event` enum. Use one variant `Event::Pathfinder(PathEvent)`
   with three cases: path found, goal reached, and pathfinding stopped.
3. Put these fields in "path found": `is_partial`, the list of nodes,
   and the time that the search used.
4. Send each event from the system that already writes the related ECS
   message.
5. Make sure that the test from step 1 passes.
6. In the port, translate `Event::Pathfinder` into `PathUpdate`,
   `GoalReached`, and `PathStop`.
7. Remove the snapshot struct, `wedge_since`, `WEDGE_GRACE`,
   `saw_success`, and `timeout_reported` from `src/pathfinder/tracking.rs`.
8. Keep the cumulative budget only if you keep upstream's "one budget
   for each goal" behavior. If you keep it, calculate it as the sum of
   the search times in the "path found" events.
9. Replace the nine wedge tests with tests on the new translation.

**Result.** The port has no heuristic for path results. The longest
function in the port is removed.

### A2. Correct the engine stop handler (P1)

**Problem.** The engine `handle_stop_pathfinding_event` does not reset
`is_calculating`. If no `ExecutingPath` exists, it also does not clear
`goal`. Thus the port calls `stop_pathfinding_patched`
(`src/pathfinder/mod.rs:857`) at four sites. This function writes the two
fields after each stop.

**Procedure.**

1. Write an engine test. Stop pathfinding while a calculation is in
   progress and no path executes. Make sure that the test shows
   `is_calculating == true` after the stop.
2. Correct `handle_stop_pathfinding_event` so that it clears `goal` and
   `is_calculating` in all cases.
3. Replace each call to `stop_pathfinding_patched` with
   `client.stop_pathfinding()` or `client.force_stop_pathfinding()`.
4. Remove `stop_pathfinding_patched`.

### A3. Correct the engine inventory packet handlers (P1) (confirm first)

**Problem.** The engine inventory plugin does not apply these packet
data. The port comments compare each case with upstream mineflayer.
Before you change the engine, compare each case with the vanilla client
too, and record the result in the test. The port writes the data into the engine `Inventory` component
from `src/windows/tracker.rs` (seven ECS writes):

| Packet data | Port site |
|---|---|
| `carried_item` in `container_set_content` | `patch_carried`, `windows/tracker.rs:524` |
| `state_id` in `set_slot` and `container_set_content` | `patch_state_id`, `windows/tracker.rs:552` |
| `c_set_player_inventory` | `windows/tracker.rs:288` |
| Window 0 `set_slot` for a slot that is not in the hotbar, while a container is open | `windows/tracker.rs:595` |
| Content that arrives before `open_screen` | `windows/tracker.rs:194` |

**Procedure.**

1. For each row in the table, write an engine simulation test. The test
   sends the packet and examines the `Inventory` component. Make sure
   that each test fails before you change the engine.
2. Correct the inventory plugin in `crates/mineflayer-client/` so that
   each test passes.
3. Move `player_inventory_slot_to_window` from the port into the engine.
4. Remove `patch_carried`, `patch_state_id`, `active_inventory_id`, and
   the ECS writes from `src/windows/tracker.rs`.
5. Keep the event logic in the tracker (`WindowUpdateSlot`,
   `HeldItemChanged`, and the closed-window remap for events).
6. After this item, the window tracker only reads the ECS. Examine the
   lock rule in `src/bot/mod.rs:118`. If no other site holds a domain
   lock while it takes the ECS lock, make the rule stricter and record
   the change in `docs/decisions.md` (D-lock-order).

**Result.** The cursor and the state id are correct for all engine
users. The bridge does not write into engine state.

### A4. A plugin hook and plugin opt-outs in the engine bootstrap (P1)

**Problem.** `src/client_app.rs` is a copy of the plugin list in the
engine's `StartClientOpts::new`. The copy exists because the engine
bootstrap has no plugin hook. `connect` also inserts
`AutoReconnectDelay::new(Duration::MAX)` to stop the auto-reconnect
plugin (`src/bot/connection.rs:343`).

**Procedure.**

1. Add an option to `StartClientOpts` (or to `Client::join`) that adds
   plugins to the app before the runner starts.
2. Add an option that disables plugins in the default plugin groups.
3. In the port, call the engine bootstrap with `DigAbortPlugin` added.
   Disable the auto-reconnect plugin and the swarm plugins.
4. Remove `src/client_app.rs` and `join_engine` in
   `src/bot/connection.rs`.
5. Remove the `AutoReconnectDelay` insert.
6. Move the test `game_tick_cancels_an_aborted_dig_through_the_production_app`
   and the test `the_production_app_installs_no_global_subscriber` so
   that they use the engine bootstrap.
7. Update D-bootstrap in `docs/decisions.md`. The port does not mirror
   the engine bootstrap any more.

### A5. A cancel for queued mining (P1)

**Problem.** Engine mining has no cancel for a queued start. Thus the
port has generation counters, an aborted-dig record, and an ECS system
that removes `MiningQueued` in `GameTick` (`src/interact/dig.rs:93-279`).

**Requirement.** Do item A4 first.

**Procedure.**

1. Write an engine test. Queue a mining start, then send the cancel.
   Make sure that the mining does not start.
2. Add a cancel to the engine mining plugin. Use a new
   `CancelMiningEvent`, or let `StopMiningBlockEvent` cancel a queued
   start.
3. In the port, send the cancel from `stop_digging` and from the dig
   abort paths.
4. Remove `reconcile_aborted_dig`, `AbortedEngineDig`, and
   `DigAbortPlugin`. Keep the parts of `InteractShared` that
   `last_dig_time` and the dig promise need.
5. Run the live suite. The interact test covers dig, stop, and place.

### A6. Engine events for the remaining re-decodes (P3)

**Problem.** The port decodes some packets again because the engine
drops them or keeps the result inside the ECS. See rows B10 to B13 in
`01_engine_boundary.md`: action bar, tab list, command suggestions,
movement events, sleep and air changes, equipment, chat session keys,
and armor attributes.

**Procedure.**

1. Do this item only after Phase B. The observer contract makes the
   change smaller.
2. For each row, decide if an engine event makes the port code shorter.
   If it does not, keep the port decode.
3. For each engine event that you add, remove the port decode and its
   per-tick diff.

## 4. Phase B: the bridge

Detail file: `docs/reviews/02_bridge_and_events.md`.

### B1. One observer contract in the bridge (P2)

**Problem.** `bridge::spawn` calls seven domain observers with seven
different signatures. The misc observer is different again: it reads
translated `BotEvent`s, not engine events, and it sends on the channel
itself.

**Requirement.** Do item F3 first.

**Procedure.**

1. Add the struct `Observe` with the client, `BotShared`, and the
   bridge-local state (movement, path, chest lids, player list).
2. Give each domain one function with this signature:
   `fn observe(cx: &mut Observe<'_>, event: &Event) -> Vec<BotEvent>`.
3. Put the observers in one constant list in `src/bridge.rs`. The list
   gives the emission order.
4. Change the misc observer so that it reads the engine event, not the
   translated `BotEvent`.
5. Make sure that the order test from F3 passes. If the order changes,
   compare it with upstream (`ref/mineflayer/lib/plugins/health.js` and
   the other plugins). Record the decision in `docs/decisions.md`
   (D-bridge).
6. Remove the six separate send loops.

### B2. The scoreboard is its own observer (P2)

**Problem.** `misc::handle_packet` sends scoreboard packets to
`scoreboard::handle_for`. The scoreboard state is in `BotShared`, not in
the misc domain.

**Procedure.**

1. Add the scoreboard to the observer list from B1.
2. Remove the scoreboard call from `misc::handle_packet`.

### B3. The misc observer takes the ECS lock only when it writes (P2)

**Problem.** `misc::handle_packet` (`src/misc/mod.rs:139`) takes the
exclusive ECS write lock for each clientbound packet. Only
`ResourcePackPush` changes the misc state. At the measured peak (8,751
events each second, about 60% raw packets, `docs/performance.md`), the
bridge takes this lock approximately 5,000 times each second.

**Procedure.**

1. Make the events from the packet without a lock.
2. Take the ECS write lock only for `ResourcePackPush`.
3. Remove the branch that inserts `MiscState` if it is missing.
   `connect` inserts it before the first packet.
4. Measure the event rate again with `examples/event_rate.rs`. Record
   the result in `docs/performance.md`.

### B4. The bridge exit is its own function (P3)

**Procedure.**

1. Move the exit logic (fallback `End`, shutdown signal, `client.exit()`)
   out of `bridge::spawn` into one function.
2. Keep the A6 test from the first plan. It must pass without change.

## 5. Phase C: shared helpers

Detail file: `docs/reviews/03_domain_modules.md`.

### C1. One ECS state accessor pair (P2)

**Problem.** Six modules have the same two functions: read a component
on the client entity, or insert a default component and change it. There
are eleven of these functions.

**Procedure.**

1. Add one generic pair: `with_component::<T>` and
   `with_component_mut::<T: Component + Default>`. Use
   `EntityWorldMut::entry::<T>().or_default()` for the insert.
2. Put the pair next to the storage rule in `src/bot/mod.rs`. If driver
   tasks have only a `Client`, put the pair on an extension trait for
   `Client`.
3. Write the lock rule one time, on the pair: the closure runs while the
   ECS lock is held, so it must not take a domain lock.
4. Replace `tool_state`, `pvp_state`, `collect_state`, `auto_eat_state`,
   `facade`, their `_mut` forms, and `misc_mut`.
5. `misc_mut` returns `Err(NotConnected)`. Keep this at its call sites
   with `.ok_or(Error::NotConnected)`.

### C2. One tick stream for the drivers (P2)

**Problem.** `follow_driver`, `dynamic_watcher`, `pvp::drive`, and
`auto_eat::drive` start with the same tick and shutdown loop.

**Procedure.**

1. Add a type `TickStream` with `async fn next(&mut self) -> bool`. It
   returns `false` when the tick channel closes or the bridge shuts down.
2. Move the explanation of the shutdown receiver from the `Bot` field
   docs to `TickStream`.
3. Change each driver to `while ticks.next().await { ... }`.
4. Keep the end report of `pvp::drive` after its loop.
5. Use `TickStream` in the `goto` wait and the `dig` wait if it makes
   them shorter.

### C3. One event wait with a typed result (P2)

**Problem.** `await_event` returns `Option`. A caller cannot tell a time
limit from a closed stream. Thus six waits are written by hand.

**Procedure.**

1. Change `await_event` to return `Result<BotEvent, WaitEnd>`. `WaitEnd`
   has the cases `Timeout`, `Ended`, and `Closed`.
2. Update the current callers. Keep their current errors.
3. Replace the hand-written waits in `interact/place.rs` (two),
   `Bot::wait_for_chunks_to_load`, `misc/creative.rs`,
   `await_window_open`, and `chat::await_message_on`.
4. For `wait_for_chunks_to_load`, use a predicate that examines the
   world on each event.

### C4. Slot event helpers in the window tracker (P3)

**Requirement.** Do item A3 first. A3 removes some of the call sites.

**Procedure.**

1. Add `WindowTracker::slot_update(window, slot, stack) -> BotEvent`.
2. Add `WindowTracker::held_changed(client) -> BotEvent`.
3. Add one function for the selected hotbar slot. Remove the three
   copies of the closure.
4. Replace the repeated blocks in `src/windows/tracker.rs`.

### C5. Remove the pass-through methods on `ChatShared` (P3)

**Problem.** `ChatShared::add_pattern`, `remove_pattern`,
`list_patterns`, `tablist`, and `store_tablist` only call the same
method on a field.

**Procedure.**

1. Make the fields `pub(crate)` or merge `ChatPatterns` into
   `ChatShared`.
2. Remove the pass-through methods.

## 6. Phase D: structure

Detail file: `docs/reviews/03_domain_modules.md`.

### D1. A generated tag lookup (P2)

**Problem.** `craft::item_tag` (`src/craft.rs:492`) is a `match` with 207
arms. It connects tag names to the static tags in the registry crate. The
table does not change when a new Minecraft version adds a tag.

**Procedure.**

1. Add `pub fn by_name(name: &str) -> Option<&'static RegistryTag<ItemKind>>`
   to `crates/mineflayer-registry/src/tags/items.rs`.
2. If the code generator is in the repository (item E1), make the
   generator write this function. If not, write it one time and record
   it in the list in item E2.
3. Write a test that finds each static tag by its name.
4. Change `item_tag` to a namespace check and one call to `by_name`.

### D2. Split the packet reducers (P3)

**Problem.** `entity::tracker::handle_packet` (217 lines),
`scoreboard::handle` (197 lines), and `world::tracker::decode_packet`
(126 lines) are each one large `match` over many packet kinds.

**Procedure.**

1. For each reducer, keep a short dispatch `match`.
2. Move each packet family into its own function. Use
   `misc::handle_packet_state` as the model.
3. Do not change behavior. The current unit tests must pass without
   change.

### D3. Shorten the remaining long functions (P3)

**Procedure.**

1. `collect::run` (159 lines): move the vacuum phase and the deposit
   phase into their own functions.
2. `auto_eat::run_eat` (130 lines): move the off-hand path into its own
   function.
3. `bridge::translate` (139 lines): move the lifecycle arms (login,
   spawn, death, disconnect) into one function.
4. Do not split `inventory::transfer::transfer`. It follows the upstream
   sequence step by step.

### D4. Move `Bot::new` to the test utilities (P2)

**Problem.** `Bot::new` is `pub` and hidden from the docs. Only unit
tests call it (34 call sites in 16 files). It is the only reason for the
field `Bot::_shutdown_tx`.

**Procedure.**

1. Move the constructor to `crate::test_util` as `pub(crate)`.
2. Remove the field `_shutdown_tx` and its documentation.
3. Record the removal in `CHANGELOG.md` under "Breaking".

### D5. One atomic component insert in `connect` (P3)

**Problem.** `BotBuilder::connect_engine` takes the ECS write lock three
times to insert `AutoReconnectDelay`, `MiscState`, and `InteractShared`.

**Procedure.**

1. Insert the components as one bundle with one write lock.
2. After item A4, remove `AutoReconnectDelay` from the bundle.

## 7. Phase E: the engine fork

Detail file: `docs/reviews/04_engine_fork.md`.

### E1. A decision about the engine code generator (P1)

**Problem.** Python scripts in the upstream `codegen/` directory make the
registry, block, item component, entity, language, and packet files.
These scripts are not in the repository.

**Procedure.**

1. Select one of these two options:
   - Copy `codegen/` into the repository and change it to write the
     `mineflayer-*` names.
   - Do not run a generator. Merge the generated files from upstream
     during each upgrade.
2. Record the decision in `docs/decisions.md` next to D-engine.
3. Update `docs/version_upgrade.md` for the selected option.

**Requirement.** Do this item before the first Minecraft upgrade.

### E2. Keep the list of local engine changes current (P1)

**Procedure.**

1. The list is in `docs/decisions.md`, after D-engine. The rename
   script is `scripts/vendor_engine.py`.
2. Add each Phase A change to the list in the same commit as the change.
3. In `docs/version_upgrade.md`, add a step: examine each listed change
   after the merge.

### E3. A publication policy for the engine crates (P3)

**Problem.** All 22 crate names were free on crates.io on 2026-10-16. To
publish `mineflayer`, you must first publish the 21 engine crates.

**Procedure.**

1. Decide if the port will be published.
2. If it will not, add `publish = false` to each engine crate. This also
   prevents a publication of `mineflayer`.
3. If it will, give the engine crates their own version number, and add
   `repository` and `description` fields.
4. Record the decision in `docs/decisions.md`.

### E4. Remove engine modules that the port does not use (P3)

**Requirement.** Do item A4 first.

**Procedure.**

1. Measure which engine modules the port reaches. Start with the swarm
   module (1,269 lines), `auto_reconnect`, `auto_respawn`,
   `accept_resource_packs`, and `auto_tool`.
2. Remove each module that the port and the engine tests do not need.
3. Run `scripts/verify.sh` and the live suite after each removal.
4. Record each removal in the list in item E2.

### E5. Engine build profile and documentation gate (P3)

**Procedure.**

1. Add `[profile.dev.package."mineflayer-*"] opt-level = 1` to
   `Cargo.toml`. Measure the compile time and the live suite time before
   and after. Keep the change only if the live suite is faster and the
   compile time is acceptable.
2. Add a gate step for the engine documentation:
   `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
   Correct the warnings first.

## 8. Phase F: tests

Detail file: `docs/reviews/05_tests_docs_tooling.md`.

### F1. The live suite in continuous integration (P2)

**Problem.** The live suite is the only test of the port and the engine
together on the wire. Continuous integration does not run it.

**Procedure.**

1. Add a second job to `.github/workflows/verify.yml`. Run it on a
   schedule and on manual dispatch.
2. In the job, install a Java 25 runtime.
3. Cache the server jar in `.live/`.
4. Run `bash scripts/live-server.sh reset`, then the live suite, then
   `bash scripts/live-server.sh stop`.
5. Keep the fixture offline. Do not connect to Mojang session servers or
   to public servers.

### F2. Engine tests for the Phase A items (P1)

Each Phase A item gives its engine test in its procedure. Write the test
first. Make sure that it fails before the engine change.

### F3. A test for the event emission order (P1)

**Problem.** The bridge sends events in an order that only the loop
shows. Item B1 changes the loop.

**Procedure.**

1. Write a unit test in `src/bridge.rs` that feeds these packets and
   records the order of the events:
   - `SetHealth` with health above 0 while the bot is dead (`Spawn` and
     `Health`).
   - `Respawn` (`Respawn`).
   - `SetActionBarText` (`Packet`, the message family, `ActionBar`).
   - `BlockEvent` for a note block (misc `NoteHeard`) and for a chest
     (world `ChestLidMove`).
2. Run the test on the current code. It must pass.
3. Do item B1 only after this test is in `main`.

## 9. Phase G: documents and tools

### G1. Move the historical documents (P3)

**Procedure.**

1. Move `docs/PORT-PLAN.md`, `docs/surveys/engine-features.md`, and
   `docs/surveys/mineflayer-api.md` into `docs/history/`.
2. Update the links in `README.md` and `docs/decisions.md`.

### G2. Remove or document the model-delegation scripts (P3)

**Problem.** `scripts/argon-unit.py` and `scripts/test-argon-unit.py`
send source files to a local model API and write the returned files into
`src/`, `docs/`, and `tests/`. They have no role in the build or the
tests.

**Procedure.**

1. Ask the maintainer if the scripts are still in use.
2. If they are not, remove them.
3. If they are, add a "Maintainer tools" section to `README.md`. Give
   the purpose and the write rules of the scripts.

### G3. Build directory size (P3)

**Problem.** `target/` measured 31 GB. The gate builds the engine crates
two times, with two different feature sets.

**Procedure.**

1. Add one sentence to the README "Toolchain" section: run `cargo clean`
   after large changes, because the build directory grows past 30 GB.
2. Do not change the gate to save space. The two builds test two real
   configurations.

## 10. Sequence and dependencies

1. F3 before B1.
2. A2 with A1 or before it.
3. A4 before A5 and before E4.
4. A3 before C4.
5. Phase B before A6.
6. E1 before the first Minecraft upgrade.
7. E2 with each engine change.

Recommended order: F3, E1, A1 with A2, A3, A4, A5, B1, B2, B3, C1, C2,
C3, D1, D4, then the P3 items.

## 11. Items that this plan does not change

| Item | Reason |
|---|---|
| One `BotEvent` enum | D-events. The size follows from the mineflayer event list. |
| The error model | The typed variants are correct (first plan, B1). |
| The lock family (`parking_lot`) | D-locks. Item A3 can make the lock order rule stricter, but the family stays. |
| One Minecraft version | The engine supports one version. Multiple versions are out of scope. |
| A new engine written from the start | The in-tree engine works and passes its tests. This plan corrects it in place. |
| The public names that follow mineflayer | The parity table is the specification. |
