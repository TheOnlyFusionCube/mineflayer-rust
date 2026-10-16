# mineflayer-rs: holistic improvement plan

Language: ASD-STE100 (Simplified Technical English).
Baseline: commit `26c4322` on `main`.
Date: 2026-10-16.

## 0. Information about this plan

### 0.1 Purpose

This plan gives the work that makes the port more correct, safer, and easier to use and maintain.
The plan does not add new mineflayer features.
The binding specification is still `docs/api-parity.md`.

### 0.2 Sources of the findings

The findings come from these sources:

1. A direct review of all production code in `src/` at the baseline commit.
2. The earlier reviews in `.superpowers/sdd/finish-port-plan/` and `/tmp/mineflayer-thermo/`.
3. Measurements: `cargo doc`, line counts, and searches of the source.

Each finding has a file and line reference.
A finding with the mark **(confirm first)** is a likely defect.
Before you change the code for such a finding, write a test that shows the defect.

### 0.3 Priority codes

| Code | Meaning |
|---|---|
| P1 | Defect. The bot can show incorrect behavior, stop, or keep a connection open after use. Do this work first. |
| P2 | Risk to users or to maintenance. Do this work after P1. |
| P3 | Quality improvement. Do this work when time is available. |

Effort is in half-days (HD). One HD is approximately four hours.

### 0.4 General warnings and cautions

**WARNING:** Do not remove the `[patch.crates-io]` table in `Cargo.toml`.
If you remove it, each `bot.attack()` call causes a server kick.
Remove the table only after azalea publishes a release that contains commit `ddef371`.

**CAUTION:** Do not change the toolchain pin in `rust-toolchain.toml`.
Do not remove `-Znext-solver=no` from `.cargo/config.toml`.
Newer nightly compilers stop with an internal compiler error on azalea.

**CAUTION:** Do not change the order of the observers in `src/bridge.rs`.
The order keeps derived events after their source packet.

**CAUTION:** Do not replace `client_app::app()` with `Client::join`.
The dig-abort plugin must be in the same app as the mining systems.

### 0.5 Rules for each work item

1. Do one work item in one commit series.
2. Write the regression test before you change the production code.
3. Run `scripts/verify.sh`. All steps must pass.
4. If the item changes behavior on the wire, run the related live test.
5. Update the related rows in `docs/api-parity.md` in the same commit.

## 1. Baseline state

| Item | Value |
|---|---|
| Production source (before the first `#[cfg(test)]`) | approximately 17,000 lines in 30 files |
| Largest production files | `pathfinder.rs` (1,886), `window_types.rs` (1,499), `entity.rs` (1,323), `inventory.rs` (1,080) |
| Unit tests | 243 tests and 2 doctests |
| Live tests | 12 test binaries, 14 tests, against a local offline server |
| Parity rows | 344 ported, 122 diverges, 49 wontfix, 11 additive |
| `BotEvent` variants | 124 |
| `Error::Operation(String)` sites | 117 |
| `.lock().unwrap()` and `.read()/.write().unwrap()` sites | 81 |
| `rustdoc` warnings | 10 |
| Continuous integration | none (no remote, no `.github/`) |
| `LICENSE` file | none |
| Size of `target/` | 35 GB |

## 2. Summary of the work items

| ID | Title | Priority | Effort |
|---|---|---|---|
| A1 | Internal waits take the bootstrap event receiver | P1 | 1 HD |
| A2 | Background drivers keep the connection open | P1 | 1 HD |
| A3 | `unequip(Hand)` can panic after a disconnect | P1 | 0.5 HD |
| A4 | `transfer` accepts a count of zero or less | P1 | 0.5 HD |
| A5 | User callbacks run while the world lock is held | P1 | 1 HD |
| A6 | The bridge can stop without an `End` event | P2 | 0.5 HD |
| A7 | `tab_complete` keeps an entry when the caller cancels | P3 | 0.5 HD |
| A8 | Skin data parse uses regular expressions on JSON | P2 | 0.5 HD |
| A9 | The entity tracker keeps old entries after a dimension change | P3 | 0.5 HD |
| B1 | Typed errors in place of `Error::Operation(String)` | P2 | 3 HD |
| B2 | `Error::Connect` holds an unrelated third-party error | P2 | 0.5 HD |
| B3 | Public signatures use azalea types that are not re-exported | P2 | 1 HD |
| B4 | Dependent crates do not receive the azalea patch | P1 | 0.5 HD |
| B5 | Stringly and sentinel return types | P3 | 1.5 HD |
| B6 | `async fn` that never waits | P3 | 0.5 HD |
| B7 | One shared event-wait helper | P2 | 1.5 HD |
| B8 | Lock poisoning policy | P2 | 1 HD |
| B9 | Lock order rule | P2 | 1 HD |
| C1 | Logs through `tracing` | P2 | 1.5 HD |
| C2 | Event volume and ring capacity | P3 | 1 HD |
| D1 | Remaining file decomposition | P3 | 5 HD |
| D2 | One convention for domain state storage | P3 | 0.5 HD |
| E1 | One live test binary with shared helpers | P2 | 1.5 HD |
| E2 | Regression tests for the A items | P1 | in each A item |
| E3 | More doctests on the main entry points | P3 | 1 HD |
| E4 | One coverage measurement | P3 | 0.5 HD |
| E5 | Live test flakes | P3 | 1 HD |
| F1 | `rustdoc` warnings and a doc gate | P2 | 0.5 HD |
| F2 | Incorrect comments | P3 | 0.5 HD |
| F3 | Package metadata, `LICENSE`, and tracked junk files | P2 | 0.5 HD |
| F4 | Continuous integration | P2 | 1 HD |
| F5 | Old planning documents | P3 | 0.5 HD |
| G1 | Performance measurements before changes | P3 | 1 HD |
| G2 | Small performance corrections | P3 | 1 HD |
| H1 | Remove the azalea patch when a release is available | P2 | 0.5 HD |
| H2 | Toolchain pin review | P3 | 0.5 HD per review |
| H3 | Reference checkout at the pinned revision | P3 | 0.5 HD |
| H4 | Procedure for the next Minecraft version | P3 | 1 HD |

Total effort: approximately 38 HD.

## 3. Phase A: correctness defects

### A1. Internal waits take the bootstrap event receiver (P1)

**Problem.**
`Bot::events()` gives the bootstrap receiver on the first call only (`src/bot/mod.rs:587`).
That receiver holds the events from before `connect` returned: `Init`, `Login`, and the spawn events.
Crate code also calls `self.events()` for its own waits.
If crate code calls it first, the crate takes the bootstrap receiver.
Then the first user subscriber does not receive `Init`, `Login`, or `Spawn`.
Also, the crate wait reads old events from the bootstrap queue.
An old `MessageStr` or `WindowOpen` can then complete the wrong wait.

Production call sites:

- `src/bot/mod.rs:671` (`wait_for_chunks_to_load`)
- `src/chat.rs:688` (`await_message`)
- `src/inventory.rs:336`, `:362`, `:411`, `:738`, `:748`, `:780`
- `src/window_types.rs:664`, `:794`, `:827`, `:1117`, `:1172`, `:1360`
- `src/craft.rs:934`, `:936`
- `src/pathfinder.rs:764` (`goto`)
- `src/misc/mod.rs:743`
- `src/collect.rs:593`

**Procedure.**

1. Write a unit test. Call `await_message` with a short timeout before the first `events()` call.
2. In the same test, call `events()`. Make sure that it receives the queued `Init` event.
3. Make sure that the test fails on the baseline code.
4. Add `pub(crate) fn subscribe(&self) -> broadcast::Receiver<BotEvent>` to `Bot`. It calls `self.shared.events.subscribe()`.
5. Replace each production call site in the list with `subscribe()`.
6. Keep `events()` for the public API only.
7. Add a comment on `events()`. The comment tells crate authors to use `subscribe()`.

**Acceptance.**

- The new test passes.
- A search for `self.events()` and `bot.events()` in production code finds no crate-internal waits.

**Status:** done. `Bot::subscribe` added; all production wait sites (including
`interact/place.rs`, `collect.rs`, and `misc/creative.rs`, which the earlier
list did not name) use it. The regression test fails on the baseline code
(it consumed the bootstrap receiver) and passes after the change.

### A2. Background drivers keep the connection open (P1) (confirm first)

**Problem.**
The `Bot` documentation says that the client disconnects when the last `Bot` handle drops (`src/bot/mod.rs:39`).
Each `Bot` clone holds a clone of `_alive: watch::Sender<()>`.
The bridge stops only when all `_alive` senders drop (`src/bridge.rs`, `alive.changed()`).
These driver tasks hold a strong `Bot`:

- `src/auto_eat.rs:288` (`drive`, runs until `disable_auto`)
- `src/pvp.rs:246` (`drive`, runs until the target goes)
- `src/pathfinder.rs:1316` (`follow_driver`)

The drivers stop when the shutdown signal fires.
The shutdown signal fires only when the bridge stops.
Thus, when auto-eat is on, the connection does not close after the user drops all handles.
The bot thread and its runtime also stay alive.

**Procedure.**

1. Write a test with `test_util::bot_with_shutdown`. Start the auto-eat driver. Drop all user handles.
2. Make sure that the test shows that the alive channel stays open.
3. Change `_alive` to `Option<watch::Sender<()>>`.
4. Add a crate-private `Bot::driver_handle()`. It clones the `Bot` with `_alive` set to `None`.
5. Give `driver_handle()` to each driver in the list.
6. Make sure that each driver still stops on the shutdown signal.

**Acceptance.**

- The test shows that the bridge stops after the last user handle drops, also when a driver runs.
- The existing driver tests pass without changes to their assertions.

**Status:** done. `_alive` is an `Option`; `Bot::driver_handle()` clears it
for the three drivers (auto-eat, pvp, pathfinder follow). The test holds a
driver-style handle in a task, drops the last user handle, and shows that
the alive channel closes (with a plain clone as the control that keeps it
open). The existing driver tests pass unchanged.

### A3. `unequip(Hand)` can panic after a disconnect (P1)

**Problem.**
`equip_empty` reads `inventory.slots().unwrap_or_default()` and then indexes `slots[QUICK_BAR_START + i]` (`src/inventory.rs:607`).
When the `Inventory` component is not available, `slots` is empty.
The index then causes a panic.
`tool.rs:347`, `tool.rs:354`, and `pathfinder.rs:1140` index slot vectors too.
Those three sites read the menu inside a component query, so the vector is never empty there.

**Procedure.**

1. Write a test with `test_util::despawned_client`. Call `unequip(EquipDest::Hand)`.
2. Make sure that the test panics on the baseline code.
3. Replace `unwrap_or_default()` with `.ok_or(Error::NotConnected)?`.
4. Search for other `slots().unwrap_or_default()` sites that index the vector. Do the same change there.

**Acceptance.** The test returns `Error::NotConnected` and does not panic.

**Status:** done. `equip_empty` uses `.ok_or(Error::NotConnected)?`. The
baseline panicked on `slots[36]` with an empty vector; the test reproduces
that and passes after the change. The other `slots().unwrap_or_default()`
sites only iterate (no indexing), so they stay.

### A4. `transfer` accepts a count of zero or less (P1)

**Problem.**
`transfer` reads `count` from `TransferOpts::effective_count()` (`src/inventory.rs:948`).
A count of zero completes at once with no action.
A negative count does not stop at zero.
The loop moves and drops items until the source has no items.
`toss`, `deposit`, and `withdraw` pass the user count to `transfer`.

**Procedure.**

1. Write a unit test for `transfer` with `count: Some(-1)` and with `count: Some(0)`.
2. At the start of `transfer`, return an error when the count is less than 1.
3. Record the behavior for zero in `docs/api-parity.md`. Compare it with upstream `transfer` first.

**Acceptance.** A negative count returns an error. No click occurs.

**Status:** done. A negative count returns
`Operation("transfer count must be >= 0")` before any click. The planned
"error when less than 1" was narrowed to negative only after the upstream
comparison step: upstream's `count === 0` branch is a cleanup-only no-op
(cursor cleanup, no clicks), and the parity row now records both
behaviors (negative = divergence, zero = upstream no-op).

### A5. User callbacks run while the world lock is held (P1) (confirm first)

**Problem.**
`WorldRef::find_blocks` with `BlockMatcher::Predicate` calls the user predicate inside `with_world` (`src/world.rs:131`).
`WorldRef::raycast` with a matcher does the same (`src/world.rs:82`).
`with_world` holds the `parking_lot` read lock on the shared world.
If the predicate reads the world again, for example with `block.world.block_at(..)`, it takes a second read lock.
A `parking_lot` read lock is not re-entrant when a writer waits.
The crate already records this hazard in `src/entity.rs:783` and `:859`.
Thus a user predicate can cause a deadlock of the bot.

**Procedure.**

1. Write a test that holds a read lock, queues a writer on another thread, and calls a predicate that reads again.
2. Change `find_matching`. Collect `(BlockPos, BlockState)` pairs under the lock.
3. Release the lock. Then run the user predicate on the collected pairs.
4. Change `raycast`. Collect the traversed `(BlockPos, BlockState)` pairs under the lock. Run the matcher after the release.
5. Keep the distance limit and the layer early exit. The collected list must stay bounded.
6. Write the lock rule on `BlockMatcher::Predicate`: "The predicate runs without a world lock."

**Acceptance.** The test completes. The results of the existing `find_blocks` and `raycast` tests do not change.

**Status:** done. `find_matching_unlocked` collects one chunk's in-range
cells under a short lock and runs the predicate after the release;
`raycast_unlocked` collects the traversed cells under one lock and runs
the matcher after the release (the collision-clip path for `None` keeps
its early exit). The regression test queues a writer while the predicate
is running: with the predicate under the lock it deadlocks (verified by
a temporary in-lock scan — the test fails with the deadlock message) and
passes now. All existing find_blocks/raycast results are unchanged.

### A6. The bridge can stop without an `End` event (P2) (confirm first)

**Problem.**
The bridge loop stops when the azalea event channel closes (`src/bridge.rs`, `let Some(event) = event else { break }`).
If the channel closes without `Event::Disconnect` or `Event::ConnectionFailed`, the bridge sends no `End`.
The broadcast sender stays alive in `BotShared`, so receivers never see `Closed`.
Waits without a timeout then wait until the shutdown signal, or forever.

**Procedure.**

1. After the loop, before `drop(shutdown)`, examine `state.ended`.
2. If `state.ended` is false, send `BotEvent::End` with the stored quit reason or `"socketClosed"`.
3. Write a test. Close the azalea channel without a disconnect event. Make sure that one `End` arrives.

**Acceptance.** Each bridge exit sends exactly one `End` event.

**Status:** done. After the loop, `spawn` emits `End` with the stored
quit reason or `"socketClosed"` when `state.ended` is false, guarded so a
`Disconnect` that already emitted stays the only `End`. The regression
test closes the azalea channel with no disconnect event: baseline never
delivers `End` (verified), the fixed bridge delivers exactly one and
retires the driver shutdown sender.

### A7. `tab_complete` keeps an entry when the caller cancels (P3)

**Problem.**
`tab_complete` removes its pending entry on timeout only (`src/chat.rs:705`).
If the caller drops the future, the entry stays in `completion_pending`.
The map grows with each cancelled call.

**Procedure.**

1. Add a small drop guard that removes the pending id.
2. Write a test that drops the future and then examines the map.

**Acceptance.** The map is empty after a cancelled call.

### A8. Skin data parse uses regular expressions on JSON (P2)

**Problem.**
`skin_data` decodes the base64 `textures` value and reads it with three regular expressions (`src/entity.rs:1265`).
The pattern `[^}]*` cannot cross a nested object.
If `metadata` comes before `url` in the `SKIN` object, the parse does not find the URL.
`serde_json` is already a direct dependency.

**Procedure.**

1. Add a test with `metadata` before `url`. Make sure that it fails on the baseline code.
2. Replace the regular expressions with a `serde_json::Value` read of `textures.SKIN.url`, `textures.SKIN.metadata.model`, and `textures.CAPE.url`.
3. Keep `base64_decode`. Its tests stay.

**Acceptance.** The old and the new skin tests pass.

**Status:** done. `skin_data` reads `textures.SKIN.url`,
`textures.SKIN.metadata.model`, and `textures.CAPE.url` through
`serde_json::Value`; the three regexes are gone. The metadata-before-url
test fails on baseline (the regex stops at the metadata's `}`) and both
skin tests pass now; `base64_decode` and its tests are untouched.

### A9. The entity tracker keeps old entries after a dimension change (P3)

**Problem.**
`EntityTracker` clears its maps on `Event::Login` only (`src/entity.rs:127`).
On a dimension change, the server does not send `RemoveEntities` for the old dimension.
Thus `known`, `equipment`, `attributes`, `effects`, and the vehicle maps keep old entries.
The maps grow with each dimension change.
Upstream also clears `bot.entities` on `login` only, so the public behavior is the same.

**Procedure.**

1. On `ClientboundGamePacket::Respawn`, clear all tracker maps except the entries for the bot itself.
2. Keep `chat_sessions` and `difficulty`. They do not belong to a dimension.
3. Write a test: add entries, send a respawn packet, and examine the map sizes.

**Acceptance.** The maps contain only the bot entries after a respawn.

## 4. Phase B: public API and error model

### B1. Typed errors in place of `Error::Operation(String)` (P2)

**Problem.**
The crate has 117 `Error::Operation(String)` sites.
A caller must compare message text to identify an error.
Upstream also uses message text, but a Rust caller expects a `match` on the variant.

**Procedure.**

1. Make a table of the 117 sites. Put each site in one group: invalid argument, wrong state, server refusal, missing item, unsupported.
2. Add one variant for each group. Each variant keeps the upstream message in a `message` field.
3. Keep the `Display` text identical to the current text. Upstream-ported code that compares text continues to work.
4. Change one domain module in each commit.
5. Keep `#[non_exhaustive]` on `Error`.

**Acceptance.**

- No `Error::Operation(` site remains, or each remaining site has a comment that gives the reason.
- All `Display` texts are unchanged. Add one test that compares texts for a sample of ten sites.

**Status:** done. `Error::Operation` is removed. 116 sites now use one of
six variants: `InvalidArgument` (bad input or wrong target kind),
`InvalidState` (the bot state does not permit the call now), `NotFound`
(a required item, block, chest, or trade is not available), `Failed`
(server refusal, no answer, or time limit), `Cancelled` (a newer call or
an explicit cancel), and `Unsupported`. The dig abort is
`DigAborted(DigAbortReason)`, so the payload names the cause. Each variant
keeps the old text as its `Display` output. The test
`error::tests::typed_errors_keep_upstream_messages` compares ten texts.
The change is one commit and not one commit for each module: the removal
of `Operation` must compile in one step.

### B2. `Error::Connect` holds an unrelated third-party error (P2)

**Problem.**
`Error::Connect` wraps `hickory_resolver::ResolveError` with `#[from]` (`src/error.rs`).
That type is a third-party type in the public API. A change in hickory is a breaking change for this crate.
`connect` also puts thread failures into this DNS type with `"...".into()` (`src/bot/mod.rs:418` and `:422`).

**Procedure.**

1. Change `Connect` to hold a `String` message or a boxed `std::error::Error`.
2. Add a `Runtime` variant for the failure to start the bot thread.
3. Keep the `#[from]` conversion as a private `From` implementation, if it is necessary.

**Acceptance.** No hickory type is in the public API of `mineflayer::Error`.

**Status:** done. `Error::Connect` holds a `String`; the `#[from]
ResolveError` is gone (the resolve site maps the error to its message),
and both bot-thread failures use the new `Error::Runtime(String)`. The
regression test pins the payload type and both display formats.

### B3. Public signatures use azalea types that are not re-exported (P2)

**Problem.**
Public functions use `BlockPos`, `EntityRef`, `Direction`, `MinecraftEntityId`, `InteractionHand`, `GameMode`, `Menu`, `FormattedText`, and other azalea types.
`src/lib.rs` re-exports only some of them. The comment at `src/lib.rs:56` says that `BlockPos` is re-exported, but it is not.
A user must add `azalea` as a direct dependency with the same version and the same patch.
The examples import `azalea::Vec3` directly.

**Procedure.**

1. Add `pub use azalea;` to `src/lib.rs`. Users then use `mineflayer::azalea::...` and receive the correct version.
2. Add a `prelude` module. Put `Bot`, `BotEvent`, `Error`, `Result`, `BlockPos`, `Vec3`, `EntityRef`, and the main option types in it.
3. Change the examples to import from `mineflayer::prelude`.
4. Correct the comment at `src/lib.rs:56`.

**Acceptance.** The examples compile without a direct `azalea::` path.

**Status:** done. `pub use azalea;` re-exports the patched crate as
`mineflayer::azalea`, and `BlockPos`, `EntityRef`, and `BlockKind` join
the root re-exports (the old comment promised `BlockPos` without
providing it). A `prelude` module carries the handle, events, errors,
the common option types, and the azalea value types in signatures; all
three examples import only `mineflayer::prelude::*` and build clean.

### B4. Dependent crates do not receive the azalea patch (P1)

**Problem.**
Cargo applies `[patch.crates-io]` only in the root workspace.
A crate that depends on `mineflayer` resolves the published azalea `0.16`.
That version has the `ServerboundAttack` defect, so `bot.attack()` causes a kick again.
The dependent crate must also use the same nightly toolchain and the `-Znext-solver=no` flag.
The README does not tell users about these conditions.

**Procedure.**

1. Add a README section with the title "Use as a dependency".
2. In that section, give the full `[patch.crates-io]` table, the toolchain file, and the `.cargo/config.toml` flag.
3. Add a dependent test crate in `scripts/` or in a temporary directory. Make sure that the instructions work.
4. Record the condition in the `Cargo.toml` comment above the patch table.

**Acceptance.** A new crate that follows the README compiles and attacks without a kick in the live fixture.

**Status:** done. The README has a "Use as a dependency" section with the
full patch table, the toolchain file, and the `.cargo/config.toml` flag,
and `Cargo.toml`'s patch-table comment points at it. Verified with a
temporary crate at `/tmp/dependent-check` (path dependency on this repo,
the three files copied from the README): it builds, joins the fixture as
two bots, attacks a spawned entity, and prints "PATCH-OK: attacked without
a kick" with no kick or end event for 4 s after the attack.

### B5. Stringly and sentinel return types (P3)

**Problem.** These public items use a string or a default value where a type is clearer:

| Item | Current | Proposed |
|---|---|---|
| `Bot::player_by_uuid(&str)` (`src/entity.rs`) | string UUID, compares `to_string()` of each player | `Uuid` argument |
| `Bot::spawn_point()` (`src/world.rs:887`) | `BlockPos::default()` before the packet | `Option<BlockPos>` |
| `Block::bounding_box()` (`src/world.rs`) | `"block"` or `"empty"` | enum, with a `as_str()` for parity |
| `Bot::click_window(slot, mouse_button: u8, mode: u8)` | integer codes | keep, and add a typed overload that takes `ClickOperation` |
| `Block::name()` | namespaced name, upstream has no namespace | keep, and add `short_name()` |

**Procedure.**

1. Change one item in each commit.
2. Update the parity row for each item. Most rows change from `ported` to `diverges` with a note.

**Acceptance.** The examples and the live tests compile with the new types.

### B6. `async fn` that never waits (P3)

**Problem.**
`update_held_item` (`src/inventory.rs:303`), `close_window` (`:447`), `activate_item` (`:630`), and `deactivate_item` (`:667`) are `async` but contain no `.await`.
A caller must use `.await` without a reason.

**Procedure.**

1. Decide one rule: keep `async` where upstream returns a `Promise`, or remove `async` where no wait occurs.
2. Record the rule in the module documentation.
3. Apply the rule to the four functions.

**Acceptance.** Each `async fn` in the public API waits for something, or has a comment with the parity reason.

### B7. One shared event-wait helper (P2)

**Problem.**
The crate has many hand-written receive loops. Each loop handles `Lagged`, `Closed`, `End`, and a timeout.
Examples: `inventory.rs` `await_window_open`, `wait_for_slot_update`, `sync_window`; `chat.rs` `await_message_on`; `bot/mod.rs` `wait_for_chunks_to_load`.
`window_types.rs:1453` already has `await_event(rx, timeout, predicate)`.

**Procedure.**

1. Move `await_event` to a crate-private module, for example `src/bot/wait.rs`.
2. Replace a loop with `await_event` only when the loop has the same terminal conditions.
3. Do not change loops with different terminal conditions. Examples: the dig wait (ticks and generations) and `wait_for_chunks_to_load` (it examines state after each event).
4. Do not make `wait_for_slot_update` call itself for each slot. The comment at `window_types.rs:1444` tells why.

**Acceptance.** The number of hand-written receive loops decreases. All existing wait tests pass.

**Status:** done. `await_event` moved to `src/bot/wait.rs`
(`crate::bot::wait`); `window_types` imports it. `sync_window` and
`wait_for_slot_update` (inventory) kept the wait's exact terminal
conditions and now call it — two hand-written loops gone. Loops with
different conditions were left alone: `wait_for_chunks_to_load` (state
check), the dig wait, the place/entity and creative waits (End keeps
waiting there, or a timeout is a distinct error), `fish` (cancel arm),
and `await_message_on` (End is not terminal upstream either). The plural
`wait_for_slot_updates` stays a single consumer loop.

### B8. Lock poisoning policy (P2)

**Problem.**
The crate has 81 `.lock().unwrap()`, `.read().unwrap()`, and `.write().unwrap()` sites on `std::sync` locks.
If one bridge observer panics while it holds a domain lock, the lock becomes poisoned.
Then each later call to that domain panics, also on user threads.

**Procedure.**

1. Decide between two options. Record the decision in `docs/PORT-PLAN.md`, as the dependency rule says.
   - Option 1: use `parking_lot::Mutex` and `RwLock`. azalea already compiles `parking_lot`. These locks have no poisoning.
   - Option 2: keep `std::sync`. Add one helper that calls `unwrap_or_else(PoisonError::into_inner)`.
2. Apply the decision to all 81 sites in one mechanical commit.
3. Write one test that panics inside a domain lock and then calls the domain again.

**Acceptance.** The test does not panic on the second call.

Note: Option 1 adds a direct dependency. Option 2 adds no dependency.

**Status:** done, Option 1 (parking_lot) — the ruling is recorded in
`docs/PORT-PLAN.md` (Constraints). Rationale: azalea's ECS lock — the one
our locks interact with — is already parking_lot, the dependency is
already in every build's graph, and "no poisoning" becomes a property of
the type (`lock()` returns the guard; there is no `unwrap()` to
reintroduce the class). All 81 single-line and the 26 multi-line
`lock()/read()/write().unwrap()` sites are now plain calls; the test
panics while holding the chat lock and then uses the domain again
(second call works; `std::sync` would panic on the poisoned lock).

### B9. Lock order rule (P2)

**Problem.**
The window observer holds the `WindowTracker` mutex and then takes `client.ecs.write()` (`src/windows/tracker.rs`, `OpenScreen`, `set_slot_events`, `SetPlayerInventory`).
`Window::close` takes `ecs.write()`, releases it, and then takes the tracker mutex.
No document gives the permitted lock order.
A future change can add the reverse order and cause a deadlock.

**Procedure.**

1. Make a list of each place that holds two locks at the same time.
2. Write one rule in `src/bot/mod.rs` next to `BotShared`. Example: "A domain lock can be held when you take the ECS lock. Do not take a domain lock while you hold the ECS lock."
3. Correct each site that does not obey the rule.
4. Add a comment at each site that holds two locks. The comment names the rule.

**Acceptance.** Each two-lock site obeys the rule and has the comment.

**Status:** done. The rule is documented next to `BotShared`
(`src/bot/mod.rs`): a domain lock may be held while the ECS lock is
taken, never the reverse, and a value needed from the ECS under a domain
lock is copied out first. Audit: the allowed direction is the window
observer (`set_slot_events`, `set_content_events`, SetHeldSlot,
EntityEvent, Cooldown, OpenScreen + the `held_signature`/`held_item`/
`active_inventory_id`/`patch_state_id` helpers), the world-state Time
arm, and the entity-tracker packet path — all commented. `Window::close`,
`SetPlayerInventory`, and `entity::players` drop the ECS guard before the
domain lock (comments name the rule). The one violation,
`pathfinder::sync_constraints`, took `CustomPathfinderState`'s lock while
holding the ECS write guard (the search thread locks that state while
holding nothing, so the orders crossed); it now copies the `Arc` out,
drops the guard, then locks. No site takes a domain lock inside
`facade`/`facade_mut` closures (noted at their definitions).

## 5. Phase C: observability

### C1. Logs through `tracing` (P2)

**Problem.**
The crate writes no logs. The parity rows `options.logErrors` and `options.hideErrors` are `wontfix` for this reason.
The bridge drops unknown azalea events and decode failures without a record (`src/bridge.rs`, `_ => vec![]`; `src/world.rs`, brand decode).
azalea already uses `tracing`, so the crate is already in the dependency graph.

**Procedure.**

1. Add `tracing` as a direct dependency. Record the ruling in `docs/PORT-PLAN.md`.
2. Add `debug!` logs for: dropped azalea events, packet decode failures, keepalive timeout, bridge exit reason.
3. Add `warn!` logs for: a domain state that is missing when the code expects it.
4. Do not log chat text, tokens, or account data at `info` or higher.
5. Change the `logErrors` and `hideErrors` parity rows. Use `diverges` with a note that names the `tracing` subscriber.

**Acceptance.** A run of `examples/echo.rs` with a `tracing` subscriber shows the bridge exit reason.

**Status:** done. `tracing` is a direct dependency (ruling in
`docs/PORT-PLAN.md`); the crate emits only, never installs a subscriber.
`debug!` logs: untranslated azalea events, brand-payload encode failure,
server keepalive timeout, and the bridge exit reason (which exit arm
fired + whether `End` was already sent). `warn!` logs: the client's
`WorldHolder` missing, and the missing pathfinder facade. No chat text,
tokens, or account data anywhere. `examples/echo.rs` installs an
`RUST_LOG`-driven `tracing-subscriber`; against the fixture with
`RUST_LOG=debug` the run logs `bridge exit reason="all Bot handles
dropped" ended=true`. The `logErrors`/`hideErrors` parity rows are now
`diverges` naming `tracing`.

### C2. Event volume and ring capacity (P3)

**Problem.**
The bridge sends one `BotEvent::Packet` for each clientbound packet.
A `ContainerSetContent` packet makes one `WindowUpdateSlot` event for each slot.
The ring has 8,192 entries (`src/bot/mod.rs:140`).
A slow subscriber receives `Lagged` and loses events. The crate cannot see this condition.

**Procedure.**

1. Do the measurement in G1 first.
2. If the measured peak is near the capacity, add a builder option to turn off `BotEvent::Packet`.
3. Write the capacity and the loss behavior in the `Bot::events` documentation.

**Acceptance.** The documentation gives the capacity and the measured peak rate.

## 6. Phase D: structure

### D1. Remaining file decomposition (P3)

The thermo review gives the seams in `/tmp/mineflayer-thermo/03_structure_bootstrap.md`.
That file is in `/tmp`. Copy it into `docs/` before the operating system removes it.

Do these extractions in this sequence. Do one extraction in one commit series.
Keep the public paths. Move the tests with their owner code.

1. `src/bot/connection.rs`: `BotBuilder`, `Auth`, `connect`, `connect_azalea`, `join_azalea`.
2. `src/pathfinder/`: `goals.rs`, `movements.rs`, and a private `tracking.rs` (`PathTracker`, `PathSnapshot`, `bridge_events`).
3. `src/window_types/`: one private child for furnace, enchantment table, anvil, and villager. Put book and command block in one child.
4. `src/entity/`: `tracker.rs` and `damage.rs`.
5. `src/world/`: `tracker.rs` (`WorldState`, `LidCounts`, `observe_event`).
6. `src/inventory/transfer.rs`: `TransferOpts`, `put_selected`, `toss_leftover`, `transfer`.

**CAUTION:** Do not change behavior during an extraction.
Do not change the tick order, the generation checks, or the observer order.

Also, in `window_types`, add one private accessor for the window record.
The code repeats `self.bot.shared.window_data.lock().unwrap().windows.get(&id)` many times.

**Acceptance.** No production file has more than approximately 1,000 lines before its tests. `scripts/verify.sh` passes after each step.

**Status:** done. The six extractions are in six commits (D1.1 to D1.6).
The thermo notes are now in `docs/reviews/`. Changes from the plan:

- The pathfinder goal child is `goal.rs`, not `goals.rs`. The parent
  imports azalea's `pathfinder::goals` module, and the two names collide.
- `window_types/book.rs` holds book editing and the command block.
- The repeated window-record lookups use one private accessor,
  `Bot::with_window_record`.

Largest production parts after the change: `pathfinder/mod.rs` (1,031
lines), `craft.rs` (972), and `world/mod.rs` (927). `craft.rs` was not in
the plan.

### D2. One convention for domain state storage (P3)

**Problem.**
Some domain state is in `BotShared` (`chat`, `world_state`, `entities`, `windows`, `window_data`, `recipes`).
Other state is in ECS components (`MiscState`, `InteractShared`, `PathfinderFacade`, `ToolState`, `PvpState`, `CollectState`, `AutoEatState`).
No document tells which location to use for new state.

**Procedure.**

1. Write the rule in the `BotShared` documentation. Example: "Put state in an ECS component when an ECS system reads it or when it must disappear with the entity. Put other state in `BotShared`."
2. Examine each current item against the rule. Move an item only if it breaks the rule.

**Acceptance.** The rule is written, and each item obeys it or has a comment with the reason.

**Status:** done. The rule is on `BotShared` (`src/bot/mod.rs`) and has
three cases: an ECS system or an ECS query reads the state, plugin state
created on first use, or `BotShared` for all other state. One item broke
the rule: `ScoreboardState` was an ECS component, but only the bridge
writes it and no azalea component is read with it. It is now
`BotShared::scoreboard`, so a scoreboard packet does not take the ECS
write lock. The four plugin states have a comment that names rule 2.

## 7. Phase E: tests

### E1. One live test binary with shared helpers (P2)

**Problem.**
`Cargo.toml` declares 12 live test binaries. Each binary links azalea, which uses much time and disk space.
Each file has its own copy of helpers such as `wait_for` and the username suffix.
The binary names mix `-` and `_` (`live-chat`, `live_misc`).

**Procedure.**

1. Make `tests/live/main.rs`. Declare each current file as a module.
2. Move the shared helpers to `tests/live/common.rs`.
3. Replace the 12 `[[test]]` entries with one entry, `name = "live"`, `required-features = ["live"]`.
4. Update the README command and `scripts/` for the new name.

**Acceptance.** `cargo test --features live -- --ignored --test-threads=1` runs all 14 tests in one binary.

**Status:** done. `tests/live/main.rs` declares the twelve former
binaries as modules; `tests/live/common.rs` carries the shared helpers
(`wait_for` — was copied in ten files, `collect_until`,
`horizontal_distance`); `Cargo.toml` has one `[[test]] live` entry; the
per-file doc comments name the one-binary command. The duplicated
`until()` variants (craft polls at 100 ms, plugins at 150 ms) and the
craft-only fresh-username `connect` stayed local — not identical. Live
run: one `tests/live/main.rs` binary, 14 passed, 116 s.

### E2. Regression tests for the A items (P1)

Each A item gives its test. Do not merge an A item without its test.

### E3. More doctests on the main entry points (P3)

**Problem.** The crate has two doctests. The main entry points have no compiled example.

**Procedure.**

1. Add a `no_run` doctest to `Bot::events`, `Bot::dig`, `Pathfinder::goto`, `Bot::open_container`, `Bot::craft`, and `Bot::await_message`.
2. Use only the `prelude` items from B3.

**Acceptance.** `cargo test --doc` runs at least eight doctests.

### E4. One coverage measurement (P3)

**Procedure.**

1. Run `cargo llvm-cov` one time on the unit tests. Do not add it to the gate.
2. Record the line coverage for each module in `docs/`.
3. Make a list of the uncovered branches in the bridge and in the A-item code.

**Acceptance.** The coverage report is in `docs/` with the date and the commit.

### E5. Live test flakes (P3)

**Problem.**
The finish report records two flakes: `live_misc` ("waiting for end" timeout) and `live-craft` ("crafting table never appeared").

**Procedure.**

1. Add diagnostic output to the two waits: the last 20 events and the elapsed time.
2. Run the full live suite ten times on a reset world. Record each failure.
3. Correct the cause. Do not add a retry loop.

**Acceptance.** Ten runs on a reset world pass without a failure.

## 8. Phase F: documentation and tools

### F1. `rustdoc` warnings and a doc gate (P2)

**Problem.**
`cargo doc --no-deps` gives 10 warnings.
Six are unresolved links to `Bot` in `src/bot/event.rs` (lines 22, 237, 440, 443, 450, 458).
Four are public documentation that links to private items (`WEDGE_GRACE`, `GetToBlockGoal`, `NearXZGoal`, `ScoreboardState`).
`scripts/verify.sh` does not build the documentation.

**Procedure.**

1. Correct the six links with `crate::Bot`.
2. Remove the four links to private items, or change them to plain code text.
3. Add a step to `scripts/verify.sh`: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.

**Acceptance.** The new verify step passes.

**Status:** done. The six `Bot` links in `src/bot/event.rs` are now
`crate::Bot` paths, and the links to private items (`Bot::subscribe`,
`WEDGE_GRACE`, `GetToBlockGoal`, `NearXZGoal`, `ScoreboardState`) are
plain code text. `verify.sh` gained a `doc` step
(`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`); it passes with zero
warnings.

### F2. Incorrect comments (P3)

**Problem.** These comments say that `parking_lot` is not a dependency. It is a dev-dependency:

- `src/lib.rs:104`
- `src/pathfinder.rs:2158`

`src/world.rs:572` is correct for normal code, but it must say "not a direct normal dependency".

**Procedure.** Correct the three comments. If B8 Option 1 is chosen, change them again in that commit.

**Status:** done. The B8 commit removed the three incorrect comments.
A search for "not a direct dependency" in `src/` finds no result.

### F3. Package metadata, `LICENSE`, and tracked junk files (P2)

**Problem.**

- `Cargo.toml` says `license = "MIT"`, but the repository has no `LICENSE` file.
- The mineflayer and azalea MIT notices are in the README only.
- `Cargo.toml` has no `repository`, `readme`, `rust-version`, or `keywords` fields.
- `docs/.DS_Store` is in git, although `.gitignore` lists `.DS_Store`.
- `target/` uses 35 GB.

**Procedure.**

1. Add `LICENSE` with the MIT text. Add the PrismarineJS and azalea notices.
2. Add the missing `Cargo.toml` fields.
3. Run `git rm --cached docs/.DS_Store`.
4. Run `cargo clean` one time after E1. Record the new size.

**Acceptance.** `cargo package --list` shows `LICENSE` and no `.DS_Store`.

**Correction (review of the F3 commit):** the first F3 commit set
`repository` to the upstream JavaScript repository and `rust-version` to
1.85. Both values were incorrect. The port has no public remote, so the
`repository` field is removed. `rust-version` is now 1.101, the version of
the pinned nightly. A stable compiler cannot build azalea.

**Status:** done, except the final `cargo clean` record. `target/` was
36 GB with the twelve live binaries and 41 GB after E1 plus the items
before it; the clean-and-rebuild measurement is deferred to the end of
the plan so the remaining items keep the warm build cache (running it
earlier would force a full azalea rebuild for every following item).
`LICENSE` carries the port notice plus the mineflayer and azalea MIT
texts; `Cargo.toml` has `readme`, `rust-version`, `keywords`, and
`categories`; `docs/.DS_Store` is untracked and deleted.
`cargo package --list --allow-dirty` lists `LICENSE` and no
`.DS_Store`.

### F4. Continuous integration (P2)

**Problem.** The repository has no remote and no CI. The gate runs only on the developer computer.

**Procedure.**

1. When a remote exists, add one CI workflow that runs `scripts/verify.sh` with the pinned toolchain.
2. Cache `~/.cargo` and `target/`.
3. Do not run the live tests in CI. They need a local server and a Java runtime.

**Acceptance.** A pull request shows the five verify steps and the doc step.

### F5. Old planning documents (P3)

**Problem.**
`docs/PORT-PLAN.md` gives "Open risks" that are now closed.
Example: the MC version decision for the fixture.
The finish reports are in `.superpowers/`, which git ignores.

**Procedure.**

1. Add a "Status" line to each closed risk in `docs/PORT-PLAN.md`.
2. Copy the decision records (R1 to R7, the azalea pin ruling) into a `docs/decisions.md` file.

**Acceptance.** Each ruling that the code depends on is in a tracked file.

## 9. Phase G: performance

### G1. Performance measurements before changes (P3)

**Procedure.**

1. Connect a bot to the reset fixture. Count the `BotEvent` variants for each second over five minutes.
2. Measure the time of `find_blocks` with `BlockMatcher::Predicate` and `max_distance` 64.
3. Measure the time of `Bot::entities()` with 200 entities.
4. Record the results in `docs/`.

**Acceptance.** The results are in `docs/` with the commit and the method.

### G2. Small performance corrections (P3)

Do these items only if G1 shows a cost.

1. `Bot::entities()` sorts by distance and then puts the result in a `HashMap` (`src/entity.rs`). The sort has no effect. Add an unsorted query for this call.
2. `entities_sorted_by_distance` takes `ecs.write()` for a read-only query. Examine whether a cached `QueryState` lets the code use a read lock.
3. `find_matching` creates a `Block`, with an `azalea::Client` clone, for each block in range. After A5, create the `Block` only for candidates that pass the distance limit.
4. `EntityTracker::forget` searches all passenger lists for each removed id. For large `RemoveEntities` packets, remove the ids in one pass.

**Acceptance.** Each change has a G1 measurement before and after it.

## 10. Phase H: dependency and platform life cycle

### H1. Remove the azalea patch when a release is available (P2)

**Procedure.**

1. Each month, examine the azalea releases for a version that contains commit `ddef371`.
2. When such a release exists, change `azalea = "0.16"` to that version.
3. Remove the `[patch.crates-io]` table.
4. Run the full gate and the `live-entity` attack test.
5. Remove the B4 README section.

### H2. Toolchain pin review (P3)

**Procedure.**

1. Each month, compile azalea with the newest nightly and without `-Znext-solver=no`.
2. If the compile passes, move the pin and remove the flag in one commit.
3. Record the result of each review in `rust-toolchain.toml`.

### H3. Reference checkout at the pinned revision (P3)

**Problem.**
`ref/azalea` is at a newer mc26.2 revision. The build uses rev `ddef371` (mc26.1).
An earlier review read the wrong source because of this difference.

**Procedure.**

1. Change `ref/azalea` to rev `ddef371`.
2. Add one line in the README that tells which revision `ref/` must have.

### H4. Procedure for the next Minecraft version (P3)

**Procedure.** Write a checklist in `docs/` for the next azalea and Minecraft version. Put these items in it:

1. Compare `client_app::app()` with `StartClientOpts::new` at the new revision.
2. Run the schedule-level test in `src/client_app.rs`.
3. Examine each packet that the bridge decodes for field changes.
4. Change the fixture server jar in `scripts/live-server.sh` to the same version.
5. Run the full live suite on a reset world.
6. Update the version rows in `docs/api-parity.md`.

## 11. Sequence and dependencies

| Step | Items | Condition |
|---|---|---|
| 1 | A1, A2, A3, A4, A5 with E2 | none |
| 2 | B4, F1, F3 | none |
| 3 | A6, A8, B2, B8, B9 | B8 before B9 |
| 4 | B3, B7, C1, E1 | B3 before E3 |
| 5 | B1 | after B2 |
| 6 | D1, D2 | after B7 and B9 |
| 7 | A7, A9, B5, B6, C2, E3, E4, E5, F2, F4, F5, G1, G2 | G2 after G1; C2 after G1 |
| Monthly | H1, H2 | none |
| Before the next MC version | H3, H4 | none |

## 12. Items that this plan does not change

The earlier reviews approved these items. Do not change them without a new defect report:

- The bootstrap mirror in `src/client_app.rs` and its schedule-level test.
- The single ordered observer loop in `src/bridge.rs`.
- The dig generation model and `reconcile_aborted_dig` in `src/interact/dig.rs`.
- `clear_inventory` with `join_all` and `CreativeSlotGuard` in `src/misc/creative.rs`.
- The explicit 21-entry azalea patch table.
- One `BotEvent` enum for all events. Do not divide it into nested event types.
- The short ECS accessor functions in each plugin module. Do not replace them with a generic state registry.
