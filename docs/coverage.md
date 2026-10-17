# Unit-test coverage measurement

Language: ASD-STE100 (Simplified Technical English).

Date: 2026-10-16. Source state: commit `2254020` (no change in `src/`
after `25d7bcb`).
Method: `cargo llvm-cov --lib --summary-only` (cargo-llvm-cov 0.9.1,
pinned nightly). The measurement includes the unit tests only. It does not
include doctests or the live tests. Coverage is not a gate (plan item E4).

Total: 4917 of 15521 lines missed, **68.32% line coverage**
(71.73% region coverage).

## Line coverage for each module (lowest first)

| Module | Lines | Missed | Cover |
|---|---|---|---|
| `src/bot/wait.rs` | 16 | 16 | 0.00% |
| `src/window_types/anvil.rs` | 185 | 175 | 5.41% |
| `src/window_types/furnace.rs` | 161 | 129 | 19.88% |
| `src/window_types/villager.rs` | 300 | 203 | 32.33% |
| `src/state.rs` | 43 | 29 | 32.56% |
| `src/window_types/enchant.rs` | 163 | 108 | 33.74% |
| `src/window_types/book.rs` | 154 | 98 | 36.36% |
| `src/inventory/transfer.rs` | 235 | 148 | 37.02% |
| `src/inventory/mod.rs` | 595 | 351 | 41.01% |
| `src/movement.rs` | 242 | 137 | 43.39% |
| `src/entity/damage.rs` | 158 | 83 | 47.47% |
| `src/interact/place.rs` | 501 | 263 | 47.50% |
| `src/collect.rs` | 671 | 350 | 47.84% |
| `src/entity/mod.rs` | 459 | 226 | 50.76% |
| `src/pathfinder/movements.rs` | 77 | 37 | 51.95% |
| `src/bot/connection.rs` | 302 | 133 | 55.96% |
| `src/windows/mod.rs` | 278 | 120 | 56.83% |
| `src/craft.rs` | 933 | 402 | 56.91% |
| `src/interact/dig.rs` | 652 | 238 | 63.50% |
| `src/misc/mod.rs` | 682 | 225 | 67.01% |
| `src/entity/tracker.rs` | 604 | 199 | 67.05% |
| `src/bot/mod.rs` | 254 | 82 | 67.72% |
| `src/windows/item.rs` | 69 | 22 | 68.12% |
| `src/misc/creative.rs` | 170 | 48 | 71.76% |
| `src/window_types/mod.rs` | 161 | 39 | 75.78% |
| `src/world/mod.rs` | 849 | 194 | 77.15% |
| `src/pathfinder/mod.rs` | 792 | 179 | 77.40% |
| `src/world/tracker.rs` | 407 | 87 | 78.62% |
| `src/tool.rs` | 419 | 88 | 79.00% |
| `src/windows/tracker.rs` | 682 | 109 | 84.02% |
| `src/pathfinder/goal.rs` | 313 | 47 | 84.98% |
| `src/bridge.rs` | 564 | 71 | 87.41% |
| `src/scoreboard.rs` | 674 | 84 | 87.54% |
| `src/chat.rs` | 627 | 76 | 87.88% |
| `src/pvp.rs` | 533 | 43 | 91.93% |
| `src/pathfinder/tracking.rs` | 560 | 36 | 93.57% |
| `src/vec3.rs` | 171 | 9 | 94.74% |
| `src/auto_eat.rs` | 717 | 32 | 95.54% |
| `src/error.rs` | 59 | 1 | 98.31% |
| `src/client_app.rs` | 31 | 0 | 100.00% |
| `src/lib.rs` | 38 | 0 | 100.00% |
| `src/mcdata.rs` | 20 | 0 | 100.00% |

Note: a file's numbers include its own `#[cfg(test)]` module.

## Interpretation

1. The low modules are the ones that send clicks and wait for server
   answers: `window_types/anvil.rs` (5%), `furnace.rs`, `villager.rs`,
   `enchant.rs`, `inventory/transfer.rs`, and `inventory/mod.rs`. Their
   pure parts (ratio math, offer decode, name limits, click mapping) have
   tests. Their wait-and-click paths need a server, and only the live
   tests exercise them.
2. `bot/wait.rs` (`await_event`) has 0% unit coverage. Every typed-window
   opener and two inventory waits use it.
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
