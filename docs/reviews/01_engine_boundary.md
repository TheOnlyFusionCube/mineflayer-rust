# 01. The port and engine boundary

Baseline `7a2bc95`. This file carries the evidence for summary finding F1.

## How the measurements were made

```sh
# engine-gap comments in the port
grep -rnE "engine (drops|doesn.t|does not|never|has no|ignores|only)|Workaround|_patched\(|wedge" src
# ECS writes into engine components from the window tracker
sed -n 1,690p src/windows/tracker.rs | grep -c 'client.ecs.write()'      # 7
# function lengths (production code only, before `mod tests`)
python3 (script in the audit session; numbers below)
```

## Catalog

Each row is a place where the port works around the engine from outside.
"Owner" is the engine crate that owns the behavior now that it is in
`crates/`.

| # | Port site | What the engine does | Port workaround | Owner |
|---|---|---|---|---|
| B1 | `src/pathfinder/tracking.rs:74` (`bridge_events`, 297 lines) | Path results are ECS messages (`PathFoundEvent`, `StopPathfindingEvent`) that never reach `Event`. | Per-tick snapshot diff of `Pathfinder`, `ExecutingPath`, and the facade; wedge heuristic (`WEDGE_GRACE`, 100 ms); cumulative search budget; timeout latch. | `mineflayer-engine` pathfinder |
| B2 | `src/pathfinder/mod.rs:857` (`stop_pathfinding_patched`) | `handle_stop_pathfinding_event` leaves `is_calculating` set, and does not clear `goal` when no `ExecutingPath` exists. | Writes `goal = None` and `is_calculating = false` after each stop. Four production call sites. | `mineflayer-engine` pathfinder |
| B3 | `src/windows/tracker.rs:524-560` (`patch_carried`, `patch_state_id`) | Ignores `carried_item` on `container_set_content`; adopts `state_id` only from non-hotbar `set_slot`. | ECS writes into `Inventory.carried` and `Inventory.state_id`. | `mineflayer-client` inventory plugin |
| B4 | `src/windows/tracker.rs:288` (`SetPlayerInventory` arm) | Drops `c_set_player_inventory`. | Writes the slot into `inventory_menu`. | `mineflayer-client` inventory plugin |
| B5 | `src/windows/tracker.rs:595` (`set_slot_events`, id 0 arm) | Applies id-0 `set_slot` only to hotbar slots while a container is open. | Writes the non-hotbar slot into `inventory_menu`. | `mineflayer-client` inventory plugin |
| B6 | `src/windows/tracker.rs:194` and `:477` | Drops content that arrives before `open_screen`; no handling for trailing syncs under a closed id. | Buffers `early_items`, writes them into the new menu; remaps closed-window syncs into window 0. | `mineflayer-client` inventory plugin |
| B7 | `src/client_app.rs` (whole file) | `Client::join` builds the app inside and gives no plugin hook. | Copies `StartClientOpts::new`'s plugin list, with a written contract to re-check it on each engine change. | `mineflayer-engine` client bootstrap |
| B8 | `src/bot/connection.rs:343` | Default plugins reconnect 5 s after any disconnect, also a local one. | Inserts `AutoReconnectDelay::new(Duration::MAX)`. | `mineflayer-engine` default plugin set |
| B9 | `src/interact/dig.rs:93-279` (`InteractShared`, `reconcile_aborted_dig`, `DigAbortPlugin`) | Mining has no cancel for a queued start. | Generation counters, an aborted-dig record, and an ECS system in `GameTick` that removes `MiningQueued`. Needs B7 to install. | `mineflayer-client` mining plugin |
| B10 | `src/bridge.rs:461` (`packet_side_events`), `src/chat.rs:7` | Drops `c_set_action_bar_text`, `c_tab_list`, `c_command_suggestions`. | Re-decodes the raw packets; synthesizes an overlay `ChatPacket::System` for the action bar. | `mineflayer-client` chat plugin |
| B11 | `src/movement.rs:404-470` | Keeps move, forced-move, and abilities changes inside the ECS. | Per-tick component diff for `Move`, packet decode for `ForcedMove`/`Abilities`. | `mineflayer-client` movement |
| B12 | `src/misc/mod.rs:100` (`tick_diffs`) | Sleep, wake, and air changes are components only. | Per-tick diff of `Pose` and `AirSupply`. | `mineflayer-entity` / client events |
| B13 | `src/entity/tracker.rs:24, :48`, `src/entity/damage.rs:42` | Drops `c_set_equipment`; `PlayerInfo` drops the chat-session key; armor attributes not stored. | Decodes the packets into `EntityTracker`. | `mineflayer-entity`, `mineflayer-client` |

B1 to B9 are the high-value cases: they either rely on a heuristic, write
into engine-owned state, or copy engine internals. B10 to B13 are
re-decodes. They are correct and cheap, and an engine event would make
them shorter, but they carry less risk.

## Why this matters now

Before `ed84001` the engine was a registry dependency. The port could
not change it, so outside patches were the only option, and the code
says so in its comments ("no upstream issue filed", "drop the component
write once a released engine clears `is_calculating`"). Those comments
describe a constraint that no longer exists.

The outside patches also cost the lock design. The rule in
`src/bot/mod.rs:118` lets a domain lock be held while the ECS lock is
taken. Almost every use of that permission is in `windows/tracker.rs`,
where the window lock is held while B3 to B6 write into the engine's
`Inventory`. If the engine applied those packets itself, the window
tracker would only read the ECS, and the lock rule could become simpler.

## Worked proposals

### P1. Pathfinder results as client events (B1, B2)

Add three variants to the engine's client `Event` enum (or one
`Event::Pathfinder(PathEvent)` with three cases): path found (with
`is_partial`, the node list, and the elapsed search time), goal reached,
and pathfinding stopped (with the reason). Send them from the systems
that already write `PathFoundEvent` and handle `StopPathfindingEvent`.
Fix the stop handler so that it clears `is_calculating` and `goal` in
all cases.

What the port then deletes: the snapshot struct, `wedge_since`,
`WEDGE_GRACE`, `search_spent`, `saw_success`, `timeout_reported`, and
most of `bridge_events`. The cumulative budget can stay if upstream's
"one budget per goal" semantics are wanted, but it becomes a sum over
reported search times, not an inference from component flicker.
`stop_pathfinding_patched` becomes `client.stop_pathfinding()`.

Proof: an engine simulation test that starts a goto to an unreachable
goal and asserts the event sequence; the existing nine wedge tests are
replaced by tests on the event translation.

### P2. Vanilla-correct inventory packet handling (B3 to B6)

In the client inventory plugin: apply `carried_item` from
`container_set_content`; apply `c_set_player_inventory` with the
`playerInventorySlotToWindow` mapping (the port's
`player_inventory_slot_to_window` moves with it); apply every id-0
`set_slot` to the player menu; adopt `state_id` from every
`set_slot` and `container_set_content` for the active menu; buffer
content that arrives before `open_screen`.

What the port then deletes: `patch_carried`, `patch_state_id`,
`active_inventory_id`, and all seven ECS writes in `windows/tracker.rs`.
The tracker keeps its job of producing `WindowUpdateSlot` and
`HeldItemChanged` events and the closed-window remap for events.

Proof: engine simulation tests for each packet (the port's existing
tracker unit tests show the expected end states and can be moved).

### P3. A plugin hook and opt-outs in the client bootstrap (B7, B8, B9)

Give `StartClientOpts` (or `Client::join`) a way to add plugins and to
disable plugins in the default groups. The port then calls the engine's
own bootstrap with `DigAbortPlugin` added and the auto-reconnect and
swarm plugins disabled. A cancel path for queued mining (a
`CancelMiningEvent`, or `StopMiningBlockEvent` honoring a queued start)
removes the need for the port's reconciler system altogether.

What the port then deletes: `client_app.rs`, the contract in
`docs/decisions.md` (D-bootstrap), the `AutoReconnectDelay` insert, and
(with the mining cancel) `reconcile_aborted_dig`, `AbortedEngineDig`, and
most of `InteractShared`.

## Risk notes

Each proposal changes the engine, so each one moves the fork further from
upstream and makes upstream merges harder (`docs/version_upgrade.md`).
Keep each engine change small, keep it in its own commit, and record it
in the fork's change list so a merge can replay it.
