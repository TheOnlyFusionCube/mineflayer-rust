# Unit-test coverage measurement

Language: ASD-STE100 (Simplified Technical English).

Date: 2026-10-18. Source state: commit `4666ad8` plus the typed-window
simulation tests (third plan TS3).
Method: `cargo llvm-cov --lib --summary-only` (cargo-llvm-cov 0.9.1,
pinned nightly). The measurement includes the unit tests only. It does not
include doctests, the live tests, or the `mineflayer-plugins` crate.
Coverage is not a gate.

Total: 2664 of 10226 lines missed, **73.95% line coverage**
(74.70% region coverage). The typed windows (`src/window_types/`) have
967 of 1124 lines covered, **86.0%**.

## Line coverage for each module (lowest first)

| Module | Lines | Missed | Cover |
|---|---|---|---|
| `src/plugin.rs` | 45 | 45 | 0.00% |
| `src/state.rs` | 43 | 25 | 41.86% |
| `src/interact/place.rs` | 502 | 264 | 47.41% |
| `src/entity/damage.rs` | 154 | 79 | 48.70% |
| `src/craft.rs` | 414 | 192 | 53.62% |
| `src/inventory/mod.rs` | 537 | 220 | 59.03% |
| `src/entity/mod.rs` | 466 | 179 | 61.59% |
| `src/movement.rs` | 242 | 86 | 64.46% |
| `src/misc/mod.rs` | 429 | 149 | 65.27% |
| `src/world/mod.rs` | 532 | 179 | 66.35% |
| `src/pathfinder/mod.rs` | 315 | 103 | 67.30% |
| `src/entity/tracker.rs` | 435 | 125 | 71.26% |
| `src/scoreboard.rs` | 317 | 91 | 71.29% |
| `src/world/tracker.rs` | 257 | 73 | 71.60% |
| `src/inventory/transfer.rs` | 235 | 65 | 72.34% |
| `src/misc/creative.rs` | 171 | 47 | 72.51% |
| `src/bot/connection.rs` | 311 | 85 | 72.67% |
| `src/bot/mod.rs` | 340 | 89 | 73.82% |
| `src/windows/item.rs` | 69 | 18 | 73.91% |
| `src/interact/dig.rs` | 442 | 105 | 76.24% |
| `src/chat.rs` | 329 | 77 | 76.60% |
| `src/window_types/villager.rs` | 310 | 72 | 76.77% |
| `src/plugins.rs` | 34 | 7 | 79.41% |
| `src/pathfinder/tracking.rs` | 172 | 35 | 79.65% |
| `src/window_types/book.rs` | 154 | 28 | 81.82% |
| `src/windows/tracker.rs` | 284 | 43 | 84.86% |
| `src/bot/wait.rs` | 63 | 9 | 85.71% |
| `src/test_util/scaffold.rs` | 57 | 8 | 85.96% |
| `src/window_types/anvil.rs` | 182 | 22 | 87.91% |
| `src/bridge.rs` | 291 | 34 | 88.32% |
| `src/window_types/enchant.rs` | 160 | 18 | 88.75% |
| `src/window_types/furnace.rs` | 157 | 16 | 89.81% |
| `src/windows/mod.rs` | 263 | 26 | 90.11% |
| `src/op.rs` | 65 | 5 | 92.31% |
| `src/test_util/golden.rs` | 188 | 9 | 95.21% |
| `src/pathfinder/movements.rs` | 230 | 9 | 96.09% |
| `src/pathfinder/goal.rs` | 239 | 7 | 97.07% |
| `src/world/block_entity.rs` | 139 | 4 | 97.12% |
| `src/test_util/sim.rs` | 242 | 5 | 97.93% |
| `src/error.rs` | 59 | 1 | 98.31% |
| `src/window_types/mod.rs` | 161 | 1 | 99.38% |
| `src/mcdata.rs` | 20 | 0 | 100.00% |

Note: a file's numbers include its own `#[cfg(test)]` module.

## Interpretation

1. The typed windows and the transfers have `BotSim` tests
   (`src/window_types/sim_tests.rs`): a test plays the server, opens the
   screen, sends the window data, and confirms each click with the slots
   the client predicted (`BotSim::echo_clicks`). The live fixture cannot
   make anvils, enchanting tables, villagers, or furnaces with data, so
   these tests are the only ones that reach those paths.
2. The plugin analogs moved to `crates/mineflayer-plugins` (third plan
   PL3); their tests run in that crate and are not in this total.
3. The reducers have good coverage: `pathfinder/tracking.rs` (94%),
   `windows/tracker.rs` (84%), `bridge.rs` (87%), `world/tracker.rs`
   (79%).

## Uncovered branches in the bridge and in the A-item code

| File | Uncovered code | Covered by |
|---|---|---|
| `bridge.rs` | The per-event body of the `spawn` loop: `Init` client information, `Login` brand payload, the observer calls, and the event fan-out | live tests only |
| `bridge.rs` | The keepalive watchdog arm (`check_timeout_interval` expiry, `Error` then `keepAliveError`) | nothing — no unit test and no live test makes the server stop keepalives |
| `bridge.rs` | `Event::ConnectionFailed` in `translate` | nothing |
| `bridge.rs` | The `debug!` arm for untranslated the engine events | nothing (log only) |
| `bot/mod.rs` (A1, A2) | `subscribe` and `driver_handle` are covered. Not covered: `quit`/`end`/`respawn`, the version getters, `registry`, `wait_for_ticks`, and `wait_for_chunks_to_load` with its chunk check | live lifecycle test |
| `inventory/mod.rs` (A3) | The `equip_empty` paths after the `NotConnected` guard: toss when the quick bar is full, and the click sequence | live tests only |
| `inventory/transfer.rs` (A4) | The click loop after the count check: the half-stack shortcut and the right-click loop | live tests only |
| `world/mod.rs` (A5) | `raycast_unlocked` with no matcher (the collision-clip path) and the `can_see_block` matcher | the existing raycast tests cover the matcher path only |
| `chat.rs` (A7) | The reply path of `tab_complete` | live chat test |
| `entity/tracker.rs` (A9) | Most packet arms (equipment, attributes, effects, links) | `forget`/respawn tests cover the maps directly |

Recommended next tests (not part of this plan): a unit test for
`await_event` (timeout, `Lagged`, and `Closed`), and a bridge test that
sends one keepalive and then no more, with a short
`check_timeout_interval`.
