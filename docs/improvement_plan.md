# mineflayer-rs: holistic improvement plan (third plan)

Language: ASD-STE100 (Simplified Technical English).
Baseline: commit `ef06f74` on `main`.
Date: 2026-10-17.

## 0. Information about this plan

### 0.1 Purpose

This plan gives the work that makes the port equal to or better than
upstream mineflayer (PrismarineJS/mineflayer 4.39.0). The plan has two
parts:

1. Structural work that makes the port simpler and removes duplicate
   state.
2. Product work that closes the capability and adoption gaps to
   upstream.

The binding API specification is still `docs/api-parity.md`.

The second plan is complete. It is in git at commit `ef06f74`
(`docs/improvement_plan.md`).

### 0.2 Sources of the findings

The findings come from the third code quality audit in `docs/reviews/`:

1. `audit_summary.md` gives the verdict and the findings F1 to F10.
2. `01_state_ownership.md` to `07_tests_docs_tooling.md` give the
   evidence, the measurements, and the worked proposals.

Each work item names its finding and its detail file. Read the detail
file before you start the work item.

### 0.3 The main idea of this plan

The port keeps a second model of the game outside the engine ECS. A
bridge task decodes each packet again after the engine applied it. Thus
the port lost the old state of each change, and it rebuilds that state
with shadow maps and a copy of the engine window state machine.

The engine runs on Bevy. It has a plugin hook
(`StartClientOpts::new_with_plugins`) and a packet message
(`ReceiveGamePacketEvent`). Phase ST moves the port packet reducers into
Bevy systems that the port owns. Then the port state is in the ECS, the
domain locks go away, and the lock-order rule goes away.

Phase OP gives all long operations one cancellation primitive and one
completion signal. Then the generation counters and the grace windows go
away.

The same plugin hook, on `BotBuilder`, is the Rust equivalent of
`bot.loadPlugin`. Phase PL makes it public and moves the five plugin
analogs onto it.

The other phases correct engine defects, add tests that run the real
engine, close capability gaps, and remove adoption barriers.

### 0.4 Work item identifiers

Each identifier has a two-letter phase code and a number.

| Code | Phase | Findings |
|---|---|---|
| DF | Engine defects | F3 |
| TS | Tests | F9 |
| ST | State ownership | F1 |
| OP | Operations | F2 |
| EV | Events and developer experience | F5 |
| PL | Plugins | F4 |
| CL | Cleanup and decomposition | F6, F7 |
| CP | Capability parity | F8 |
| AD | Adoption | F10 |

**NOTE:** Do not write these identifiers in code comments. Write the
rule in the comment. Write the identifier in the commit message. Plan
identifiers become incorrect when a new plan replaces this plan.

### 0.5 Priority codes

| Code | Meaning |
|---|---|
| P1 | The item corrects a defect, removes duplicate state, or is a prerequisite for a P1 item. Do this work first. |
| P2 | The item removes repeated code, closes a frequent parity gap, or removes an adoption barrier. Do this work after the P1 items that it depends on. |
| P3 | The item is a quality improvement or a less frequent parity gap. Do this work when time is available. |

Effort is in half-days (HD). One HD is approximately four hours.

### 0.6 General warnings and cautions

**WARNING:** Do not change the order of the `BotEvent`s that one engine
event produces before item TS2 is complete. Users can depend on this
order. A changed order can cause incorrect bot behavior without a test
failure.

**WARNING:** Do not connect a bot to a Mojang endpoint or to a public
server during this work. Use only the local offline-mode fixture
(`scripts/live-server.sh`).

**CAUTION:** Do not change the toolchain pin in `rust-toolchain.toml`.
Do not remove `-Znext-solver=no` from `.cargo/config.toml`. Item AD1 is
the only item that can change them, and only after decision DEC-1.

**CAUTION:** Each change to `crates/` makes the next upstream merge more
difficult. Keep each engine change small. Put each engine change in its
own commit. Record each engine change in the D-engine list in
`docs/decisions.md`.

**CAUTION:** Do not rename game data that contains the word "azalea"
(blocks, sound events, tags, language strings). It is Minecraft data.

**CAUTION:** A Bevy system that the port adds runs while the engine
holds the ECS world. Do not block, sleep, or wait in such a system.

### 0.7 Rules for each work item

1. Do one work item in one commit series.
2. Write the regression test before you change the production code.
   After item TS1, use the simulation harness for each test that needs
   packets.
3. Run `scripts/verify.sh`. All steps must pass.
4. If the item changes behavior on the wire, reset the fixture and run
   the live suite:
   `bash scripts/live-server.sh reset`, then
   `cargo test --features live --no-fail-fast -- --ignored --test-threads=1`.
5. If the item changes a public API, update the related rows in
   `docs/api-parity.md` and `CHANGELOG.md` in the same commit.
6. If the item changes the engine, add an engine-level test. Use the
   simulation harness in `crates/mineflayer-client/src/test_utils/`.
7. If the item removes code, record the measured line count before and
   after in the commit message.

### 0.8 Decisions that the owner must make

Some items need a decision that is difficult to reverse. Each decision
has a recommendation. Record each decision in `docs/decisions.md` before
you start the items that depend on it.

| ID | Question | Recommendation | Items that depend on it |
|---|---|---|---|
| DEC-1 | Must the port and the engine build on stable Rust? | Yes. A dependent must copy a nightly pin and a rustflag today. This is the largest adoption barrier. Seven local feature gates cause it. Do the AD0 spike first. Each upstream merge can bring the gates back, so include that recurring cost in the decision. | AD1, AD2 |
| DEC-2 | How must the crates be published? | Publish all crates under one version line and keep the current crate layout. A merge of the engine crates into one crate conflicts with the upstream directory layout that the upgrade process uses. | AD2 |
| DEC-3 | Where must the plugin analogs go? | One `mineflayer-plugins` workspace crate with one cargo feature for each plugin. The core crate re-exports it behind default features. | PL3, PL4 |
| DEC-4 | Must the port support more than one Minecraft version? | No. Keep one version. Make the upgrade to the next version fast and tested (`docs/version_upgrade.md`). | none |
| DEC-5 | Can phases ST and OP change the engine? | Yes, for the pre-apply hook (ST2), the operation identifiers (OP2, OP5), and the defects (DF). Each change is small and is in the D-engine list. | ST2, OP2, OP5 |

## 1. Baseline state

| Item | Value |
|---|---|
| Port source | 26,234 lines in 43 files; about 18,000 lines before the test modules |
| Engine source (`crates/`) | 122,132 lines in 21 crates |
| Port functions longer than 100 lines | 5 of 782 |
| Port files longer than 1,000 lines | 11 (all because of in-file tests) |
| Port unit tests | 254, in 0.35 s |
| Port tests that use the engine `Simulation` | 0 |
| Live tests | 14 in one binary; CI runs them daily and by hand |
| Line coverage | 68.32% (`docs/coverage.md`) |
| Locks in `BotShared` | 9 |
| `lock rule:` comments | 16 |
| Cancellation and generation sites | 145 in 13 files |
| `BotEvent` variants | 123 (15 of them for plugin analogs) |
| Engine game-packet handlers that only log | 22 |
| Engine enums with `#[non_exhaustive]` | 6 |
| Parity rows | 342 ported, 126 diverges, 47 wontfix, 11 additive |
| Ported examples | 3 (upstream: 48 scripts, 10 projects) |
| Minecraft versions | 1 (upstream: 28) |
| Toolchain | nightly-2026-10-01 and `-Znext-solver=no`, also for dependents |
| Published crates | 0 |
| Size of `target/` | 42 GB |

## 2. Target state

The plan is complete when all of these conditions are true. Each
condition has a check.

| Condition | Check |
|---|---|
| No known packet is dropped because of a chat component. | The DF1 tests pass. |
| A game packet that fails to decode makes a `BotEvent`. | The DF2 test passes. |
| The port has no domain state outside the ECS, apart from the event sender and the quit reason. | `BotShared` has at most two fields with a lock. |
| The lock-order rule is gone. | `grep -rn "lock rule" src` finds nothing. |
| `blockUpdate` gives the correct old block on the first update of a position. | The ST3 test passes. |
| Generation counters exist only in the operation primitive. | `grep -rnE "goal_gen|set_gen|dig_seq|aborted_seq|eat_generation" src` finds nothing. |
| A third-party crate can write each plugin analog with the public API only. | `mineflayer-plugins` builds with no `pub(crate)` access to `mineflayer`. |
| Users have a public `once`-style wait. | `Bot::wait_for` is in the rustdoc. |
| The plugin rows of the parity table are not `wontfix`. | `docs/api-parity.md` rows for `loadPlugin`, `loadPlugins`, `hasPlugin`, `options.plugins`. |
| The typed windows have tests on each push. | Coverage of `src/window_types/` is 80% or more. |
| Line coverage of the port is 80% or more. | `cargo llvm-cov --lib --summary-only`. |
| The bridge event order has a golden test. | TS2 passes. |
| At least 20 upstream examples are ported. | `ls examples/*.rs`. |
| The port builds on stable Rust (if DEC-1 is yes). | CI builds with `stable`. |
| Memory and CPU per bot are measured against upstream. | `docs/performance.md` has the table from AD5. |

## 3. Summary of the work items

| ID | Title | Finding | Priority | Effort | Depends on |
|---|---|---|---|---|---|
| DF1 | Chat components become text, not decode errors | F3 | P1 | 2 HD | none |
| DF2 | A `BotEvent` for each game packet that fails to decode | F3 | P1 | 0.5 HD | none |
| DF3 | Remove `#[non_exhaustive]` from the owned engine enums | F3 | P1 | 0.5 HD | none |
| DF4 | A callback for the Microsoft device code | F3 | P2 | 0.5 HD | none |
| DF5 | Correct three pathfinder engine details | F3 | P2 | 0.5 HD | none |
| TS1 | The port simulation harness `BotSim` | F9 | P1 | 3 HD | none |
| TS2 | Golden event sequences for the bridge | F9 | P1 | 1 HD | TS1 |
| TS3 | Simulation tests for typed windows and transfers | F9 | P2 | 2 HD | TS1 |
| TS4 | A live smoke subset on each push | F9 | P3 | 0.5 HD | none |
| TS5 | Export the harness for plugin authors | F9 | P3 | 0.5 HD | TS1, PL2 |
| ST1 | The port plugin group and the event queue | F1 | P1 | 1.5 HD | TS2 |
| ST2 | A pre-apply hook in the engine | F1 | P1 | 1 HD | ST1 |
| ST3 | The world domain in the ECS | F1 | P1 | 2 HD | ST1 |
| ST4 | The window domain in the ECS | F1 | P1 | 3 HD | ST2 |
| ST5 | The entity domain in the ECS | F1 | P1 | 3 HD | ST2 |
| ST6 | Scoreboard, chat, misc, recipes, and window data in the ECS | F1 | P2 | 3 HD | ST1 |
| ST7 | Remove the observers, the domain locks, and the lock-order rule | F1 | P1 | 1 HD | ST3 to ST6 |
| OP1 | One operation primitive | F2 | P1 | 1.5 HD | TS1 |
| OP2 | Goto identifiers in the engine | F2 | P1 | 1.5 HD | OP1 |
| OP3 | `goto`, `set_goal`, and `stop` on goto identifiers | F2 | P1 | 2 HD | OP2 |
| OP4 | Follow and dynamic goals as ECS systems | F2 | P1 | 2 HD | OP3 |
| OP5 | Mining identifiers and a mining result message | F2 | P1 | 2 HD | OP1 |
| OP6 | Eat, auto-eat, collect, pvp, and creative on the primitive | F2 | P2 | 2 HD | OP1, OP5 |
| EV1 | Public wait helpers | F5 | P1 | 0.5 HD | none |
| EV2 | An event stream that counts lost events | F5 | P2 | 1 HD | EV1 |
| EV3 | Typed event payloads | F5 | P2 | 1 HD | TS2 |
| EV4 | Chat and spawn helpers | F5 | P3 | 0.5 HD | EV1 |
| PL1 | `BotBuilder::plugin` and plugin switches | F4 | P1 | 1 HD | ST1 |
| PL2 | The public plugin module | F4 | P1 | 1.5 HD | OP1, PL1 |
| PL3 | Move `tool`, `auto_eat`, `collect`, and `pvp` to `mineflayer-plugins` | F4 | P2 | 3 HD | PL2, OP6, DEC-3 |
| PL4 | Move `pathfinder` to `mineflayer-plugins` | F4 | P3 | 2 HD | PL3, OP4 |
| PL5 | Document the plugin model | F4 | P2 | 0.5 HD | PL3 |
| CL1 | Move large test modules to sibling files | F7 | P2 | 0.5 HD | none |
| CL2 | One block scan, one eye position, one raycast | F6 | P2 | 1 HD | none |
| CL3 | One mine-progress scorer | F6 | P2 | 0.5 HD | none |
| CL4 | Goal cleanups | F6 | P3 | 1 HD | OP4 |
| CL5 | Locks and copies in hot paths | F6 | P3 | 0.5 HD | none |
| CL6 | Correct the stale text | F6 | P2 | 0.5 HD | none |
| CL7 | Small local simplifications | F6 | P3 | 0.5 HD | none |
| CP1 | Block entities and sign text | F8 | P1 | 3 HD | ST3 |
| CP2 | Honor the `Movements` fields | F8 | P2 | 4 HD | OP4 |
| CP3 | Block placement in the pathfinder | F8 | P2 | 6 HD | CP2 |
| CP4 | Mutable physics settings and a physics switch | F8 | P2 | 2 HD | PL1 |
| CP5 | Resource packs in both phases, and a denial that works | F8 | P2 | 1 HD | PL1 |
| CP6 | A static recipe table | F8 | P3 | 3 HD | none |
| CP7 | A `login` event for each login packet | F8 | P3 | 0.5 HD | DF3 |
| CP8 | Chat languages other than `en_us` | F8 | P3 | 1 HD | DF1 |
| AD0 | A stable-toolchain spike | F10 | P1 | 0.5 HD | none |
| AD1 | Build on stable Rust | F10 | P1 | 3 HD | AD0, DEC-1 |
| AD2 | Publish the crates | F10 | P2 | 1 HD | AD1, DEC-2 |
| AD3 | Port 20 upstream examples | F10 | P2 | 5 HD | EV1 |
| AD4 | A porting guide and hosted API docs | F10 | P2 | 1 HD | EV1, PL5 |
| AD5 | A resource benchmark against upstream | F10 | P2 | 1.5 HD | none |
| AD6 | Repository hygiene | F10 | P3 | 0.25 HD | none |

Total effort: approximately 85 HD (about 43 working days for one person).

## 4. Sequence

The items form these chains. An item can start when the items it depends
on are complete.

1. DF1, DF2, DF3, DF4, DF5, TS1, TS4, EV1, CL1, CL2, CL3, CL5, CL6,
   CL7, AD0, AD5, and AD6 have no dependency. Start them in parallel.
2. TS1 → TS2 → ST1 → (ST2, ST3, ST6) → (ST4, ST5) → ST7.
3. TS1 → OP1 → (OP2, OP5) → OP3 → OP4. OP5 → OP6. OP1 can run beside
   phase ST. OP3 to OP6 change the same files as ST4 and ST5, so start
   them after ST7.
4. ST1 → PL1. OP1 and PL1 → PL2 → PL3 → PL4 → PL5.
5. ST3 → CP1. OP4 → CP2 → CP3. PL1 → CP4, CP5.
6. AD0 → DEC-1 → AD1 → AD2.

**NOTE:** Do not start CL items that change a file that ST or OP will
rewrite. CL2, CL3, CL5, and CL6 touch only code that ST and OP keep.
CL4 waits for OP4.

## 5. Work items: engine defects (DF)

### DF1. Chat components become text, not decode errors

Finding: F3. Detail: `05_engine_defects.md`. Priority: P1. Effort: 2 HD.

Files: `crates/mineflayer-chat/src/component.rs`,
`crates/mineflayer-chat/src/text_component.rs`.

Procedure:

1. Write a test that decodes a `ClientboundSetTitleText` whose text is
   the NBT compound `{"keybind": "key.jump"}`. Make sure that the test
   fails at the baseline.
2. Write tests for `score`, `selector`, `object`, and `nbt` components in
   the NBT form and in the JSON form. Put one of each inside the `extra`
   list of a text component.
3. In `FormattedText::from_nbt_compound`, keep each of the five kinds as
   its own component that holds the raw data. Do not convert it to plain
   text, because `mc_write` must write the same component back. Give each
   kind a display fallback that matches vanilla:
   - `keybind`: the key identifier (for example `key.jump`).
   - `selector`: the selector pattern.
   - `score`: the `value` field if it is present, else an empty string.
   - `nbt`: an empty string.
   - `object`: an empty string.
4. Keep the style and the `extra` children of the component.
5. Do the same in the JSON `Deserialize` path.
6. Make sure that `FormattedText::mc_read` returns an error only for NBT
   that is not a text component at all.
7. Write a round-trip test: read each kind, write it, read it again, and
   compare.
8. Update the `ChatMessage` row in `docs/api-parity.md`.

Done when: all tests in steps 1, 2, and 7 pass, and a container whose title
has a keybind opens in a `BotSim` test (after TS1).

### DF2. A `BotEvent` for each game packet that fails to decode

Finding: F3. Detail: `05_engine_defects.md`. Priority: P1. Effort: 0.5 HD.

Files: `crates/mineflayer-client/src/plugins/connection.rs`,
`crates/mineflayer-engine/src/events.rs`, `src/bridge.rs` (or the ST1
plugin), `src/bot/event.rs`.

Procedure:

1. In `handle_raw_packet`, when `deserialize_packet` fails in the game
   state, send `Event::PacketDecodeFailed { packet_id, error }` to the
   local-player event sender. Keep the `error!` log.
2. Translate it to `BotEvent::Error` with a message that names the
   packet id.
3. Write a test that sends bytes that do not decode and expects the
   event.

Done when: the test passes, and the event shows in a live run if a
decode fails.

### DF3. Remove `#[non_exhaustive]` from the owned engine enums

Finding: F3. Detail: `05_engine_defects.md`. Priority: P1. Effort: 0.5 HD.

Files: `crates/mineflayer-engine/src/events.rs`,
`crates/mineflayer-engine/src/pathfinder/goto_event.rs`,
`crates/mineflayer-engine/src/pathfinder/mod.rs`, `src/bridge.rs`,
`src/pathfinder/tracking.rs`.

Procedure:

1. Remove `#[non_exhaustive]` from `Event`, `PathEvent`, and the four
   other types that `grep -rn non_exhaustive crates/mineflayer-engine/src`
   shows.
2. Remove the wildcard arm from `bridge::translate` and from
   `tracking::bridge_events`. Write one arm for each variant.
3. Record the change in the D-engine list.

Done when: a new variant in `Event` causes a compile error in the port.

### DF4. A callback for the Microsoft device code

Finding: F3. Detail: `05_engine_defects.md`. Priority: P2. Effort: 0.5 HD.

Files: `crates/mineflayer-auth/src/auth.rs`, `src/bot/connection.rs`.

Procedure:

1. Add a callback parameter to the device-code function. The callback
   gets the verification URI and the user code.
2. Make the current `println!` the default callback.
3. Add an optional callback to `Auth::Microsoft`.
4. Change the `options.onMsaCode` row from `wontfix` to `ported`.

Done when: a unit test receives the code through the callback, and the
library writes nothing to stdout when a callback is given.

### DF5. Correct three pathfinder engine details

Finding: F3. Detail: `05_engine_defects.md`. Priority: P2. Effort: 0.5 HD.

Files: `crates/mineflayer-engine/src/events.rs`,
`crates/mineflayer-engine/src/pathfinder/mod.rs`.

Procedure:

1. Correct the doc of `PathEvent::PathFound`. An empty path with
   `is_partial == true` means that no route exists or that the budget
   ran out.
2. Move the 100 ms sleep after a partial result out of `calculate_path`
   into the repath loop that needs it. Then `Pathfinder::get_path_from_to`
   does not sleep the caller.
3. Replace the `expect("path should always still be possible …")` with a
   log and a `None` return.

Done when: the engine tests pass, and a test shows that
`get_path_from_to` with a partial result returns in less than 100 ms.

## 6. Work items: tests (TS)

### TS1. The port simulation harness `BotSim`

Finding: F9. Detail: `07_tests_docs_tooling.md`. Priority: P1. Effort: 2 HD.

Files: `src/test_util/` (new directory, from the `test_util` module in
`src/lib.rs`), `crates/mineflayer-client/src/lib.rs`,
`crates/mineflayer-client/src/test_utils/simulation.rs`.

**CAUTION:** `mineflayer-client` compiles `test_utils` only with its
`log` feature (`crates/mineflayer-client/src/lib.rs:11-13`). The port
must not turn on `log` (D-log), and the port test
`the_production_app_installs_no_global_subscriber` fails if it does.

Procedure:

1. Put `test_utils` behind a new `test-utils` feature of
   `mineflayer-client` that does not turn on `log`. Re-export it through
   `mineflayer-engine`. Turn the feature on only in the port
   `[dev-dependencies]`.
2. Make sure that `Simulation` can build its app from `ClientPlugins`
   plus extra plugins (the port plugins of ST1 must run in it). If it
   cannot, add a constructor that takes a `PluginGroup`.
3. Move `test_util` from `src/lib.rs` to `src/test_util/mod.rs`.
4. Add `src/test_util/sim.rs` with `BotSim`. `BotSim` holds an engine
   `Simulation`, a `Bot` over the simulation's client, and an event
   receiver.
5. Give `BotSim` these methods: `new` (login and spawn packets),
   `receive`, `tick`, `events` (drain the events since the last call),
   and `sent` (the serverbound packets).
6. Make sure that the real bridge runs in `BotSim`. If the bridge needs
   the engine event channel, add a way to get it from `Simulation`.
7. Write three tests: login gives `Connected`, `Login`, `Spawn`; a chat
   packet gives `Chat`, `Message`, `MessageStr`; a block update gives
   `BlockUpdate`.

Done when: the three tests pass in `cargo test` with no network.

### TS2. Golden event sequences for the bridge

Finding: F9. Detail: `07_tests_docs_tooling.md`. Priority: P1. Effort: 1 HD.

**WARNING:** This item is the safety net for phase ST. Do not start ST1
before this item is complete.

Procedure:

1. Write a packet script that has: login, two chunks, a block update and
   a section update, an entity spawn, move, metadata, and removal, a
   window open, fill, slot change, and close, a scoreboard objective and
   score, a team, a boss bar, a title, a death, and a respawn.
2. Run the script through `BotSim` at the baseline. Write the event
   sequence (variant names and the key fields) to
   `tests/golden/bridge_events.txt`.
3. Write a test that runs the script and compares the result with the
   golden file.
4. Keep `bridge_emission_order` until ST7. Then remove it, because the
   golden test covers it.

Done when: the golden test passes at the baseline.

### TS3. Simulation tests for typed windows and transfers

Finding: F9. Detail: `07_tests_docs_tooling.md`. Priority: P2. Effort: 2 HD.

Procedure:

1. For each typed window (furnace, enchanting table, anvil, villager,
   book, command block), write a `BotSim` test. Send the packets that a
   vanilla server sends. Examine the events and the serverbound clicks.
2. Write `BotSim` tests for `transfer`, `deposit`, and `withdraw` on a
   chest window.
3. Measure the coverage again. Update `docs/coverage.md`.

Done when: the coverage of `src/window_types/` is 80% or more.

### TS4. A live smoke subset on each push

Finding: F9. Detail: `07_tests_docs_tooling.md`. Priority: P3. Effort: 0.5 HD.

Procedure:

1. Add a `live-smoke` job to `.github/workflows/verify.yml` that runs on
   push. It runs the `lifecycle`, `chat`, and `movement` live tests.
2. Use the same jar cache as the `live` job.

Done when: the job passes on a push.

### TS5. Export the harness for plugin authors

Finding: F9. Priority: P3. Effort: 0.5 HD.

Procedure:

1. Put `BotSim` behind a `test-util` cargo feature in addition to
   `cfg(test)`.
2. Use it in the tests of `mineflayer-plugins` (PL3).

Done when: `mineflayer-plugins` tests use `BotSim` through the feature.

## 7. Work items: state ownership (ST)

**CAUTION:** In phase ST, move one domain in one commit series. After
each series, the golden test of TS2 must pass with no change to the
golden file. If the golden file must change, stop and record why in the
commit message.

### ST1. The port plugin group and the event queue

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 1.5 HD.

Files: `src/plugins/mod.rs` (new), `src/bot/connection.rs`,
`src/bridge.rs`.

Procedure:

1. Add `PortPlugins`, a Bevy `PluginGroup`. At first it has one plugin,
   `BotEventPlugin`.
2. `BotEventPlugin` adds a `BotEventQueue` component to the client
   entity, and a system that drains the queue into the broadcast sender
   at the end of each `Update` pass.
3. In `connect_engine`, call `StartClientOpts::new_with_plugins` with
   `ClientPlugins` and `PortPlugins`.
4. Move the lifecycle and chat translation of the engine `Event`s into a
   system that reads the same ECS messages that the engine converts into
   `Event` (spawn, death, chat, tick, disconnect). Then every `BotEvent`
   comes from a system, and the system order is the event order.

   **WARNING:** Do not let the async bridge task and the drain system
   both send events. The bridge runs outside the schedule. Its events
   and the queue events of the same pass then go out in an order that
   changes from run to run.
5. Run TS2. If the golden file must change, record the new order as a
   public change in `CHANGELOG.md` and get approval first.

Done when: TS2 passes, and all events go out through `BotEventQueue`.

### ST2. A pre-apply hook in the engine

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 1 HD.

Files: `crates/mineflayer-client/src/plugins/connection.rs`,
`docs/decisions.md`.

Procedure:

1. Add a `PacketPreApply` resource. It holds a list of
   `fn(&mut World, Entity, &ClientboundGamePacket)`.
2. In `handle_raw_packet`, call each function in the list before
   `game::process_packet`.
3. Write an engine test: a hook sees an entity before a
   `ClientboundRemoveEntities` packet despawns it.
4. Record the change in the D-engine list.

Done when: the engine test passes. The hook is approximately 15 lines.

### ST3. The world domain in the ECS

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 2 HD.

Files: `src/plugins/world.rs` (new), `src/world/mod.rs`,
`src/world/tracker.rs`.

Procedure:

1. Write a `BotSim` test: the first block update of a position gives the
   correct old block. Make sure that it fails at the baseline.
2. Add `WorldStatePlugin`. Its resources or components are `WorldTime`
   (clocks and game time), `Weather`, `SpawnPoint`, `GameInfo` (hardcore,
   max players, flat, difficulty, server brand), and `ChestLids`.
3. Add a system that reads `ReceiveGamePacketEvent` and updates these.
   Order it so that the events go out in the TS2 order.
4. Add a system that reads `QueuedServerBlockUpdates` before the engine
   applies them. Order it with
   `.before(mineflayer_client::block_update::handle_block_update_event)`;
   the function is public, so no engine system set is necessary. It reads the old state from the world, and pushes
   `BlockUpdate { old, new }`.
5. Change the `Bot` world methods to read the new components.
6. Remove `WorldState`, `last_block_updates`, `MAX_TRACKED_UPDATES`,
   `LidCounts`, and `world::observe_event`.
7. Change the `event('blockUpdate')` row from `diverges` to `ported`.

Done when: the test of step 1 passes, and TS2 passes.

### ST4. The window domain in the ECS

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 3 HD.

Files: `src/plugins/windows.rs` (new), `src/windows/`,
`crates/mineflayer-client/src/plugins/inventory/mod.rs`.

Procedure:

1. Add a pre-apply hook (ST2) for `ContainerSetContent`,
   `ContainerSetSlot`, `SetPlayerInventory`, `OpenScreen`, and
   `ContainerClose`. The hook copies the old contents of the affected
   slots into a `PendingSlotChanges` component.
2. Add a system that reads `PendingSlotChanges` and the engine
   `Inventory`, and pushes `WindowUpdateSlot`, `HeldItemChanged`,
   `SetWindowItems`, `WindowOpen`, and `WindowClose`.
3. Read the window lifecycle (pending, early content, last closed) from
   the engine `Inventory`. Do not keep a copy.
4. Move the `copyInventory` step of `Window::close` into the engine
   close handler. Then the port does not write the engine `Inventory`.
5. Move `eat_generation` and `eat_notify` out of `WindowTracker`. OP6
   replaces them.
6. Remove `WindowTracker.pending`, `early_items`, `last_closed`, `slots`,
   and the functions `set_content_events` and `set_slot_events`.

Done when: TS2 and TS3 pass, and `grep -n "early_items\|last_closed" src`
finds nothing.

### ST5. The entity domain in the ECS

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 3 HD.

Files: `src/plugins/entity.rs` (new), `src/entity/`.

Procedure:

1. Add components on each entity: `Equipment`, `AttributeSnapshots`,
   `MobEffects`, `Vehicle`, `Passengers`, and `ChatSessionKeys` (on the
   player entities).
2. Add a system that updates them from the packets. Bevy removes them
   when the entity despawns.
3. Get the metadata events (`EntityCrouch`, `EntityUncrouch`,
   `EntityElytraFlew`, `EntitySleep`) from the typed engine components
   (`AbstractEntityShiftKeyDown`, `FallFlying`, `SleepingPos`, `Pose`).
   Do not read metadata by index.
4. Add a pre-apply hook for `RemoveEntities`. It pushes `EntityGone` with
   a live `EntityRef`, and `Dismounted` when the entity is the bot's
   vehicle.
5. Remove `EntityTracker`, `forget`, `forget_other_entities`, and the
   `known` map.
6. Change the `event('entityGone')` row: `entity` is no longer `Option`.

Done when: TS2 passes, and `src/entity/tracker.rs` has no `HashMap`
keyed by `MinecraftEntityId`.

### ST6. Scoreboard, chat, misc, recipes, and window data in the ECS

Finding: F1. Detail: `01_state_ownership.md`,
`03_bridge_and_events.md`. Priority: P2. Effort: 3 HD.

Procedure:

1. Move `ScoreboardState` to a `Scoreboard` component and a
   `ScoreboardPlugin`.
2. Move the chat state (`TabList`, the tab-list player set, the
   tab-complete replies, the chat patterns) to `ChatStatePlugin`. Move
   the chat decode out of `bridge::translate` and
   `bridge::packet_side_events`.
3. Move the misc decode (titles, sounds, particles, resource packs,
   note blocks) to `MiscStatePlugin`. Decode `BlockEvent` in one system
   for pistons, chest lids, and note blocks.
4. Get `Sleep`, `Wake`, and `Breath` from Bevy change detection on
   `Pose` and `AirSupply`. Remove the per-tick ECS write lock.
5. Move `RecipeBook` and `WindowDataTracker` to components.

Done when: TS2 passes, and `BotShared` has no domain tracker.

### ST7. Remove the observers, the domain locks, and the lock-order rule

Finding: F1. Detail: `01_state_ownership.md`. Priority: P1. Effort: 1 HD.

Procedure:

1. Remove `OBSERVERS`, `Observe`, and the observer functions that are
   now empty.
2. Keep in the bridge only the lifecycle translation (`Init`, `Login`,
   `Spawn`, `Death`, `Disconnect`, `ConnectionFailed`), the keepalive
   watchdog, and the brand payload. Move the brand payload and the
   watchdog into a `ConnectionPlugin` if that is shorter.
3. Reduce `BotShared` to the event sender, `quit_reason`, and
   `end_reason`.
4. Remove the lock-order rule from `src/bot/mod.rs` and every
   `lock rule:` comment.
5. Update D-lock-order, D-bridge, D-redecode, and D-storage in
   `docs/decisions.md`.
6. Measure the event latency and rate with `examples/event_rate.rs`.
   Record the result in `docs/performance.md`.

Done when: the target-state conditions for `BotShared` and the lock rule
are true, and the live suite passes on a reset world.

## 8. Work items: operations (OP)

### OP1. One operation primitive

Finding: F2. Detail: `02_async_operations.md`. Priority: P1. Effort: 1.5 HD.

Files: `src/op.rs` (new), `src/bot/mod.rs`, `Cargo.toml`.

Procedure:

1. Add `tokio-util` to the port dependencies (it is already a workspace
   dependency).
2. Give `Bot` a root `CancellationToken`. The bridge cancels it when it
   exits.
3. Add `OpSlot` with `start` (cancel the old token, return a child of
   the root) and `cancel`.
4. Replace `Bot::shutdown`, `TickStream`'s shutdown arm, and
   `driver_handle`'s shutdown purpose with the root token. Keep the
   alive sender, or replace it with a drop guard if that is shorter.
5. Write tests: a new `start` cancels the old token; the bridge exit
   cancels every token.

Done when: the tests pass, and `TickStream` has no shutdown field.

### OP2. Goto identifiers in the engine

Finding: F2. Detail: `02_async_operations.md`. Priority: P1. Effort: 1.5 HD.

Files: `crates/mineflayer-engine/src/pathfinder/`,
`crates/mineflayer-engine/src/events.rs`.

Procedure:

1. Make `start_goto_with_opts` return the goto identifier (the engine
   already counts gotos in `Pathfinder::goto_id`).
2. Add the identifier to each `PathEvent` variant.
3. Add an optional total time budget to `PathfinderOpts`. When the sum
   of search times for one goto is more than the budget, the engine ends
   the goto with a timeout result.
4. Write engine tests for steps 2 and 3.
5. Record the change in the D-engine list.

Done when: the engine tests pass.

### OP3. `goto`, `set_goal`, and `stop` on goto identifiers

Finding: F2. Detail: `02_async_operations.md`. Priority: P1. Effort: 2 HD.

Procedure:

1. Make `goto` wait for the `PathEvent`s that have its own identifier.
   Remove the tick poll.
2. Remove `set_gen`, `last_failure`, and `PathTracker`. Use the engine
   budget of OP2 for `think_timeout`.
3. Keep the upstream rules that have tests today: a satisfied goal emits
   `goal_updated` then `goal_reached`; a timeout keeps the goal
   installed; an equal goal does not reject a pending `goto`.

Done when: all pathfinder unit tests and the pathfinder live tests pass,
and `bridge_events` in `tracking.rs` is a translation with no state.

### OP4. Follow and dynamic goals as ECS systems

Finding: F2. Detail: `02_async_operations.md`. Priority: P1. Effort: 2 HD.

Procedure:

1. Add a `FollowTarget { entity, range }` component and a system that
   issues a radius goal when the target moves to a new block, and stops
   with `PathStop` when the target despawns.
2. Add a `DynamicGoal` component and a system that issues the goal again
   when it stops being satisfied.
3. Order both systems after the engine pathfinder systems.
4. Remove `follow_driver`, `dynamic_watcher`, `pathfinder_live`,
   `cancel_watcher`, `goal_gen`, and `watcher_cancel`.
5. Remove the orphan doc text above `follow_driver`.

Done when: the follow and dynamic tests pass in `BotSim`, and the
pathfinder and pvp live tests pass.

### OP5. Mining identifiers and a mining result message

Finding: F2. Detail: `02_async_operations.md`. Priority: P1. Effort: 2 HD.

Files: `crates/mineflayer-client/src/plugins/mining.rs`,
`src/interact/dig.rs`.

Procedure:

1. Give `StartMiningBlockEvent` an identifier. Add a `MiningFinished {
   id, outcome }` message with the outcomes `Broken`, `Cancelled`, and
   `Interrupted`.
2. Make `dig` wait for its own `MiningFinished` and for the air block.
   Remove `dig_seq`, `aborted_seq`, the grace counters, and
   `cancel_engine_dig`.
3. Keep `target_dig_block`, `target_dig_face`, and `last_dig_time`.
4. Record the engine change in the D-engine list.

Done when: the dig unit tests and the interact live tests pass.

### OP6. Eat, auto-eat, collect, pvp, and creative on the primitive

Finding: F2. Detail: `02_async_operations.md`. Priority: P2. Effort: 2 HD.

Procedure:

1. Add a port system that emits an `EatFinished` message on entity
   status 9, on a cooldown for the held item, and on a change of the held
   stack.
2. Make `consume` and `auto_eat().eat` use an `OpSlot` and wait for
   `EatFinished`. Remove `eat_generation`, `eat_notify`, and the
   auto-eat cancel flags.
3. Make `collect`, `pvp().attack`, and `creative().set_inventory_slot`
   use an `OpSlot`. Remove their generation numbers and `AtomicBool`
   flags.

Done when: the target-state condition for generation counters is true.

## 9. Work items: events and developer experience (EV)

### EV1. Public wait helpers

Finding: F5. Detail: `03_bridge_and_events.md`. Priority: P1. Effort: 0.5 HD.

Procedure:

1. Make `WaitEnd` public.
2. Add `Bot::wait_for(timeout, predicate) -> Result<BotEvent, WaitEnd>`
   and `Bot::wait_map(timeout, f) -> Result<T, WaitEnd>` over the
   existing `await_event` and `await_map`.
3. Add a doctest that is the Rust form of `await once(bot, 'spawn')`.
4. Use the helpers in the examples.

Done when: the doctest compiles, and no example has a hand-written wait
loop for one event.

### EV2. An event stream that counts lost events

Finding: F5. Detail: `03_bridge_and_events.md`. Priority: P2. Effort: 1 HD.

Procedure:

1. Add `Bot::event_stream() -> impl Stream<Item = BotEvent>`.
2. When the receiver lags, put a `BotEvent::Lagged(n)` item in the
   stream. Do not hide the loss.
3. Change the examples to `while let Some(event) = stream.next().await`.

Done when: a test makes the receiver lag and gets `Lagged(n)`.

### EV3. Typed event payloads

Finding: F5. Detail: `03_bridge_and_events.md`. Priority: P2. Effort: 1 HD.

**CAUTION:** This item changes the public API. Record each change in
`CHANGELOG.md`.

Procedure:

1. Replace `PathReset { reason: &'static str }` with an enum
   `PathResetReason { GoalUpdated, MovementsUpdated }`.
2. Replace `Kicked(String, bool)` with
   `Kicked { reason: String, logged_in: bool }`.
3. Replace `PistonMove { direction: u8 }` with the engine `Direction`.
4. Key boss bars by `Uuid`. Make `ResourcePack.uuid` a `Uuid`.
5. Remove `HardcodedSound`.
6. Use `EntityRef` (imported) in place of `mineflayer_engine::EntityRef`.
7. Update the golden file of TS2 in the same commit.

Done when: the gate passes and the golden diff shows only these renames.

### EV4. Chat and spawn helpers

Finding: F5. Priority: P3. Effort: 0.5 HD.

Procedure:

1. Add `Bot::chat_messages()`, a stream of `(sender, message)` items,
   the same values as upstream's `chat` event.
2. Add `Bot::spawned()`, which resolves on the first `Spawn`.

Done when: `examples/echo.rs` uses both and is shorter.

## 10. Work items: plugins (PL)

### PL1. `BotBuilder::plugin` and plugin switches

Finding: F4. Detail: `03_bridge_and_events.md`, `05_engine_defects.md`.
Priority: P1. Effort: 1 HD.

Procedure:

1. Add `BotBuilder::plugin(impl Plugin)`. Pass the plugins to
   `new_with_plugins` after `PortPlugins`.
2. Add `BotBuilder::respawn(bool)`. `false` disables
   `AutoRespawnPlugin` in the group.
3. Add `BotBuilder::accept_resource_packs(bool)`. `false` disables
   `AcceptResourcePacksPlugin`. CP5 completes the denial.
4. Write a `BotSim` test for each switch.
5. Change the `options.respawn` row to `ported`.

Done when: the tests pass.

### PL2. The public plugin module

Finding: F4. Detail: `03_bridge_and_events.md`. Priority: P1. Effort: 1.5 HD.

Procedure:

1. Add the public module `mineflayer::plugin`.
2. Make these public in it: `Bot::state::<T>()` and
   `Bot::state_mut::<T>()` (over `with_component` and
   `with_component_mut`), `Bot::start_op` (over `OpSlot`), a tick stream
   that stops on the root token, and `Bot::plugin_events::<E>()` (a
   typed broadcast for each plugin event type `E`).
3. Write a small test plugin in `tests/` that uses only this module. It
   keeps a counter, reacts to ticks, and emits its own event.

Done when: the test plugin builds and passes with no `pub(crate)` item.

### PL3. Move `tool`, `auto_eat`, `collect`, and `pvp` to `mineflayer-plugins`

Finding: F4. Detail: `03_bridge_and_events.md`. Priority: P2. Effort: 3 HD.

Prerequisite: decision DEC-3.

Procedure:

1. Add the workspace crate `crates/mineflayer-plugins` (or the layout
   that DEC-3 selects). Give each plugin a cargo feature.
2. Move the four modules. Use only `mineflayer::plugin` and the public
   `Bot` API. If a move needs a private item, add the item to
   `mineflayer::plugin` first, in its own commit.
3. Move the plugin events (`EatStart`, `EatFail`, `EatFinish`,
   `CollectBlockFound`, `Collected`, `CollectDone`,
   `CollectBlockFinished`, `StartedAttacking`, `StoppedAttacking`,
   `AttackedTarget`) out of `BotEvent` into one event type for each
   plugin.
4. Re-export the plugins from `mineflayer` behind default features, so
   that `mineflayer::pvp` and the others still work.
5. Move the plugin tests. Use `BotSim` through the `test-util` feature
   (TS5).

Done when: the gate and the `plugins` live tests pass.

### PL4. Move `pathfinder` to `mineflayer-plugins`

Finding: F4. Priority: P2. Effort: 2 HD.

Procedure:

1. Do the steps of PL3 for `pathfinder`.
2. Move the five path events out of `BotEvent`.

Done when: the gate and the pathfinder live tests pass.

### PL5. Document the plugin model

Finding: F4. Priority: P2. Effort: 0.5 HD.

Procedure:

1. Change the `loadPlugin`, `loadPlugins`, `hasPlugin`, `options.plugins`,
   and `loadInternalPlugins` rows from `wontfix` to `diverges`, with the
   note "a Bevy plugin added at build time; no loader at run time".
2. Add a "Writing a plugin" section to the README with the test plugin
   of PL2.

Done when: the README section compiles as a doctest.

## 11. Work items: cleanup and decomposition (CL)

### CL1. Move large test modules to sibling files

Finding: F7. Detail: `04_domain_modules.md`. Priority: P2. Effort: 0.5 HD.

Procedure:

1. For each file whose test module has more than 300 lines, move the
   module to a sibling `tests.rs` and write `#[cfg(test)] mod tests;`.
2. Make sure that no file in `src/` is longer than 1,000 lines.

Done when: the gate passes, and no `src/` file is longer than 1,000
lines.

### CL2. One block scan, one eye position, one raycast

Finding: F6. Detail: `04_domain_modules.md`. Priority: P2. Effort: 1 HD.

Procedure:

1. Replace `find_states` and `find_matching_unlocked` with one
   `spiral_find` that takes the per-chunk step as a closure. Remove
   `euclidean`.
2. Keep one eye-position helper. Remove `dig_eye_pos` and one of
   `eye_pos` and `eye_and_look`.
3. Keep one raycast over `traverse_blocks` with a visitor argument.
   Remove `raycast_hit`.
4. Remove the second default of `point` in `Bot::find_blocks`.

Done when: the world and dig tests pass with no change.

### CL3. One mine-progress scorer

Finding: F6. Detail: `04_domain_modules.md`. Priority: P2. Effort: 0.5 HD.

Procedure:

1. Add `Bot::mine_progress(stack, block) -> f32` with one ECS query.
2. Use it in `Pathfinder::best_harvest_tool`, `Tool::best_tool_for`, and
   `Bot::dig_time`.
3. Keep the tie rule and the bare-hand candidate as parameters, or use
   one rule if the upstream plugins agree. Record the choice in the
   parity table.

Done when: the tool, pathfinder, and dig tests pass.

### CL4. Goal cleanups

Finding: F6. Detail: `04_domain_modules.md`. Priority: P3. Effort: 1 HD.

Procedure:

1. Derive `PartialEq` for `Goal`. Compare `Follow` entities by id.
   Remove `same_goal`.
2. Add `impl<G: Goal + ?Sized> Goal for Arc<G>` in the engine. Remove
   `SharedGoal`.
3. Move `AnyGoal`, `AllGoal`, `InvertedGoal`, `GetToBlockGoal`, and
   `NearXZGoal` into the engine `goals` module. Use one octagonal xz
   distance helper.

Done when: the goal tests pass.

### CL5. Locks and copies in hot paths

Finding: F6. Detail: `04_domain_modules.md`. Priority: P3. Effort: 0.5 HD.

Procedure:

1. If ST6 is not complete, make `misc::tick_diffs` use a read lock and
   keep its baselines in the bridge state.
2. Split the `ResourcePackPush` arm from the stateless misc decode.
   Remove `&mut MiscState::default()`.
3. Make `Window::slot` read one slot under the component guard. Do not
   clone the `Menu`.
4. Remove the `warn!` in `with_world`, or log it once.

Done when: the tests pass.

### CL6. Correct the stale text

Finding: F6. Detail: `04_domain_modules.md`. Priority: P2. Effort: 0.5 HD.

Procedure:

1. Correct the `windows` module header and the `menu_for_id` doc. The
   engine applies `carried_item` and `set_player_inventory` now.
2. Correct `src/interact/mod.rs`. `InteractShared.dig_target` is the
   source of `targetDigBlock`.
3. Remove the orphan text above `follow_driver`
   (`src/pathfinder/mod.rs:795`), if OP4 is not complete.
4. Correct the 13 rename leftovers ("no the engine", "; The engine").
5. Replace the 33 plan-identifier citations in comments with the rule
   that each one names.
6. Remove the empty `impl Bot {}` in `src/window_types/mod.rs`.

Done when: `grep -rnE "no the engine|\((first plan )?[A-H][0-9]+\)|\b[NM]-[0-9]+\b|\bU[0-9]{1,2}\b" src`
finds nothing.

### CL7. Small local simplifications

Finding: F6. Detail: `04_domain_modules.md`. Priority: P3. Effort: 0.5 HD.

Procedure:

1. Make `open_container` call `open_block` for the block case.
2. Replace `pvp::rand_next` with the `rand` crate.
3. Replace the `is_none` and `unwrap` pair in `craft_once` with a
   `match` or `get_or_insert`.
4. Replace `is_container_block_name` with a `BlockKind` set.

Done when: the tests pass.

## 12. Work items: capability parity (CP)

### CP1. Block entities and sign text

Finding: F8. Detail: `06_parity_and_ecosystem.md`. Priority: P1. Effort: 3 HD.

Procedure:

1. Store the block-entity NBT from the chunk packets and from
   `block_entity_data` in a `BlockEntities` map for each chunk. Remove
   the entries when the chunk unloads or the block changes.
2. Add `Block::block_entity()` and `Bot::sign_text(block, back)`.
3. Write `BotSim` tests for a sign in a chunk packet and for a sign
   update.
4. Change the `updateSign` and `Block` rows.

Done when: a live test reads the text of a sign that the bot wrote.

### CP2. Honor the `Movements` fields

Finding: F8. Detail: `04_domain_modules.md`, `06_parity_and_ecosystem.md`.
Priority: P2. Effort: 4 HD.

**WARNING:** A `Movements` field that the pathfinder ignores can make a
bot walk off a cliff or into lava. Do not keep a field that has no
effect.

Procedure:

1. Implement `max_drop_down` in the descend moves.
2. Implement `can_open_doors` with a door move that sends the use
   action.
3. Implement `allow_sprinting` as a flag to the path executor.
4. Implement `dont_mine_under_falling_block` and `dont_create_flow` as
   break-cost filters.
5. Remove `allow_free_motion`, or mark it `wontfix` with a reason.
6. If the engine `PathfinderOpts` can carry the constraints directly,
   remove `MovementConstraints`, the `CustomPathfinderState` use, and
   `sync_constraints`.

Done when: each field has a test that shows its effect.

### CP3. Block placement in the pathfinder

Finding: F8. Priority: P2. Effort: 6 HD.

Procedure:

1. Add a scaffold-block list to `Movements`.
2. Add moves that place a block: pillar up, and bridge across a gap.
3. Add `Goal::PlaceBlock` and make `Pathfinder::is_building` true while
   the executor places.
4. Test in `BotSim` and in a live test with a gap.

Done when: a live bot crosses a two-block gap with scaffolding.

### CP4. Mutable physics settings and a physics switch

Finding: F8. Priority: P2. Effort: 2 HD.

Procedure:

1. Put the physics constants in a `PhysicsSettings` resource that the
   physics systems read.
2. Add `Bot::set_physics`.
3. Add `BotBuilder::physics(bool)`. Test that the movement plugins work
   with physics disabled. If they do not, keep the switch out and record
   why.

Done when: a test changes gravity and sees the effect.

### CP5. Resource packs in both phases, and a denial that works

Finding: F8. Priority: P2. Effort: 1 HD.

Procedure:

1. Read `ReceiveConfigPacketEvent` in a port system. Emit `ResourcePack`
   for configuration-phase pushes.
2. With `accept_resource_packs(false)` (PL1), answer each push only from
   `accept_resource_pack` and `deny_resource_pack`.
3. Change the three resource-pack rows.

Done when: a `BotSim` test denies a push and sees the decline packet.

### CP6. A static recipe table

Finding: F8. Priority: P3. Effort: 3 HD.

Procedure:

1. Extract the recipes from the vanilla data reports at upgrade time.
   Write them as a generated file in the port.
2. Let `recipes_for` and `recipes_all` use the table and the recipe
   book. Add a parameter that selects the source.
3. Add the extraction to `docs/version_upgrade.md`.

Done when: `recipes_for` finds a recipe that the server did not unlock.

### CP7. A `login` event for each login packet

Finding: F8. Priority: P3. Effort: 0.5 HD.

Procedure:

1. Emit `Event::Login` for each `ClientboundLogin`.
2. Change the `event('login')` row.

Done when: a `BotSim` test with two login packets gets two `Login`
events.

### CP8. Chat languages other than `en_us`

Finding: F8. Priority: P3. Effort: 1 HD.

Procedure:

1. Let `BotBuilder` take a language map.
2. Use it for translatable components.

Done when: a test renders a translatable component with a custom map.

## 13. Work items: adoption (AD)

### AD0. A stable-toolchain spike

Finding: F10. Detail: `06_parity_and_ecosystem.md`. Priority: P1. Effort: 0.5 HD.

Procedure:

1. Run `cargo +stable check --workspace` on a branch. Record each error.
2. Sort the errors into the seven feature gates and any dependency that
   needs nightly (for example `simdnbt` or a Bevy crate).
3. Write the result and an effort estimate for AD1 into DEC-1.

Done when: DEC-1 has the list of errors and the estimate.

### AD1. Build on stable Rust

Finding: F10. Detail: `06_parity_and_ecosystem.md`. Priority: P1. Effort: 3 HD.

Prerequisite: AD0 and decision DEC-1.

Procedure:

1. Replace the two `trait_alias` aliases in `mineflayer-physics` with
   explicit bounds.
2. Replace the three `default fn` sites in `mineflayer-buf` and
   `mineflayer-inventory` with a marker trait or a macro.
3. Remove the thiserror `#[backtrace]` field use, or capture a
   `Backtrace` without the `provide` API.
4. Change `FastFixedBitSet<const N: usize>` so that the const parameter
   is the word count.
5. Write out the one type-changing struct update in `mineflayer-engine`.
6. Remove `-Znext-solver=no`. If the build fails, find the cause and
   correct it.
7. Change `rust-toolchain.toml` to a stable release. Remove the
   dependent-crate instructions from the README.
8. Record each change in the D-engine list and in
   `docs/version_upgrade.md`, so that an upgrade merge can do it again.

Done when: CI builds and tests on stable.

### AD2. Publish the crates

Finding: F10. Priority: P2. Effort: 1 HD.

Prerequisite: AD1 and decision DEC-2.

Procedure:

1. Add a public repository and the `repository` field.
2. Remove `publish = false`. Give the engine crates their
   `description` fields.
3. Publish the crates in dependency order.
4. Change D-publish.

Done when: `cargo add mineflayer` works in a new crate, and docs.rs
shows the API.

### AD3. Port 20 upstream examples

Finding: F10. Detail: `06_parity_and_ecosystem.md`. Priority: P2. Effort: 5 HD.

Procedure:

1. Port these upstream examples: `inventory`, `sleeper`, `fisherman`,
   `farmer`, `guard`, `trader`, `book`, `anvil`, `scoreboard`, `titles`,
   `tab_complete`, `blockfinder`, `raycast`, `sound`, `multiple`,
   `reconnector`, `pathfinder`, `collectblock`, `auto-eat`, and
   `attack`.
2. Use the EV helpers in each example.
3. Port `reconnector` as upstream does it: make a new `Bot` after
   `BotEvent::End`. The engine has no auto-reconnect (D-engine item 15),
   and upstream has none either.
4. Add a live test for each example that the fixture can run.

Done when: `cargo build --examples` builds 24 or more examples.

### AD4. A porting guide and hosted API docs

Finding: F10. Priority: P2. Effort: 1 HD.

Procedure:

1. Write `docs/porting.md`. Map `bot.on`, `once`, `loadPlugin`, the
   common options, and the common errors to the Rust API.
2. Publish the `cargo doc` output from CI (a static site or an
   artifact) until AD2 is complete.

Done when: the guide has a working example for each mapping.

### AD5. A resource benchmark against upstream

Finding: F10. Detail: `06_parity_and_ecosystem.md`. Priority: P2. Effort: 1.5 HD.

Procedure:

1. Write a benchmark that connects 1, 10, and 100 idle bots to the local
   fixture. Measure the resident memory and the CPU time for each count.
2. Do the same measurement with upstream mineflayer from `ref/mineflayer`.
3. Record both tables in `docs/performance.md`.
4. If the thread for each bot is the limit at 100 bots, add a work item
   for many bots on one runtime.

Done when: `docs/performance.md` has both tables.

### AD6. Repository hygiene

Finding: F10. Detail: `07_tests_docs_tooling.md`. Priority: P3. Effort: 0.25 HD.

Procedure:

1. Add `.omo/` to `.gitignore`.
2. Give the gate steps with the engine `log` feature their own
   `CARGO_TARGET_DIR`, so that one target directory can be removed
   without a full rebuild.

Done when: `git status` is clean after a gate run.

## 14. Record of this plan

At the end of each work item, update the "Status" column below. Write
the commit that completes the item.

| ID | Status | Commit |
|---|---|---|
| DF1 | done | `bae2f84` |
| DF2 | done | `370344a` |
| DF3 | done | `fe527b4` |
| DF4 | done | `4455b4e` |
| DF5 | done | `ee1afa8` |
| TS1 | done | `eb342dc` |
| TS2 | done | (this commit) |
| TS3 | done; `src/window_types/` 86% line coverage, the crate 74% (the 80% crate target of section 2 is not met) | (this commit) |
| TS4 | done; the job's first push run is not observed yet (no remote) | (this commit) |
| TS5 | done: `tests/plugin.rs` uses `BotSim`, and `mineflayer-plugins` uses the exported scaffolds, through the feature | (this commit) |
| ST1 | replaced by D-port-plugins: the bridge stays, `BotPlugins` added | (ST3 commit) |
| ST2 | done | (this commit) |
| ST3 | done | (this commit) |
| ST4 | done; `pending` stays (the engine keeps no "windowOpen not sent yet" state); `eat_generation` and `eat_notify` wait for OP6 | `5b5537f`, (this commit) |
| ST5 | done; `EntityGone.entity` stays `Option`: an id the engine never spawned has no entity (upstream invents a blank one) | (this commit) |
| ST6 | done | (this commit) |
| ST7 | done; `OBSERVERS` stays as the emission order (D-port-plugins) | (this commit) |
| OP1 | done | (this commit) |
| OP2 | done | (this commit) |
| OP3 | done | (this commit) |
| OP4 | done; follow and dynamic goals step in the bridge on each `Tick`, not in an ECS system (D-port-plugins: the bridge is the one sender, and a goal step must read the facade between passes) | (this commit) |
| OP5 | done; a non-`Broken` outcome counts as a break when the block is air | (this commit) |
| OP6 | done; the eat stays beside `using_held_item` in the window tracker (the held-item packets end it); `creative().set_inventory_slot` keeps its per-slot in-flight set, which rejects a second call for the slot, because the supersede rule of `OpSlot` would change that | (this commit) |
| EV1 | done | `a8f77a5` |
| EV2 | done | (this commit) |
| EV3 | done | (this commit) |
| EV4 | done | (this commit) |
| PL1 | done | (this commit) |
| PL2 | done | (this commit) |
| PL3 | done; DEC-3 decided: no re-export from the core (dependency cycle) | (this commit) |
| PL4 | not done, by decision: the pathfinder translates engine path events and steps its goals inside the bridge, and a plugin crate has no hook into the engine event stream; a public hook only for this move would be surface with one user (DEC-3) | |
| PL5 | done | (this commit) |
| CL1 | done | (this commit) |
| CL2 | done | (this commit) |
| CL3 | done | (this commit) |
| CL4 | done; `AnyGoal`, `AllGoal`, and `InvertedGoal` are the engine's `OrGoals`, `AndGoals`, and `InverseGoal` over `Arc<dyn Goal>`, not moved copies | (this commit) |
| CL5 | done | `3a39167`, (this commit) |
| CL6 | done | `e67a4be` |
| CL7 | done | (this commit) |
| CP1 | done; the port keeps the block entities (D-redecode), not the engine | (this commit) |
| CP2 | done; `dont_create_flow` and `allow_free_motion` are removed (the engine always refuses to break next to a liquid and has no free motion); `MovementConstraints` stays, because `SuccessorsFn` is a plain `fn` | (this commit) |
| CP3 | open | |
| CP4 | done; three constants are settable (gravity, air drag, airborne inertia), the others are refused with `Unsupported`; the physics switch works (BotSim test) | (this commit) |
| CP5 | done; the configuration-phase reply has no `BotSim` test (the harness runs the game state) | (this commit) |
| CP6 | open | |
| CP7 | done | (this commit) |
| CP8 | done | (this commit) |
| AD0 | done; DEC-1 waits for the owner | (this commit) |
| AD1 | open | |
| AD2 | open | |
| AD3 | done; 24 examples build; no per-example live test (the examples wait for a player's chat), `titles` checked by hand on the fixture | (this commit) |
| AD4 | done; the CI artifact is not observed yet (no remote) | (this commit) |
| AD5 | open | |
| AD6 | done | `4659c99`, (this commit) |
