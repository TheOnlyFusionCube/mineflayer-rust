# 04. Domain modules: duplication, stale text, file size

Findings F6 and F7 in [`audit_summary.md`](audit_summary.md). Baseline
`ef06f74`.

## Measurements

Function length (a brace-depth scan of every `fn` before
`#[cfg(test)]`, script kept in the session scratchpad): 782 production
functions, 5 over 100 lines, 26 over 60.

| Lines | Function |
|---|---|
| 161 | `src/windows/tracker.rs:178` `observe_event` |
| 152 | `src/pathfinder/tracking.rs:40` `bridge_events` |
| 141 | `src/inventory/transfer.rs:109` `transfer` |
| 121 | `src/bridge.rs:54` `spawn` |
| 110 | `src/interact/dig.rs:166` `dig_opts` |
| 99 | `src/entity/tracker.rs:480` `metadata_update` |
| 97 | `src/pathfinder/mod.rs:274` `goto` |

F1 deletes most of `observe_event` and `metadata_update`. F2 deletes most
of `bridge_events` and half of `goto` and `dig_opts`. Only `transfer` and
`spawn` need a local split.

File size (total lines, then lines before `#[cfg(test)]`):

| File | Total | Production | Test |
|---|---|---|---|
| `src/world/mod.rs` | 1390 | 957 | 433 |
| `src/pathfinder/mod.rs` | 1335 | 960 | 375 |
| `src/bridge.rs` | 1288 | 517 | 771 |
| `src/misc/mod.rs` | 1179 | 803 | 376 |
| `src/craft.rs` | 1175 | 777 | 398 |
| `src/auto_eat.rs` | 1154 | 595 | 559 |
| `src/chat.rs` | 1129 | 725 | 404 |
| `src/scoreboard.rs` | 1072 | 590 | 482 |
| `src/collect.rs` | 1052 | 807 | 245 |
| `src/inventory/mod.rs` | 1034 | 869 | 165 |
| `src/entity/tracker.rs` | 1002 | 638 | 364 |

Every file over 1,000 lines is over because of its in-file test module.
Move each test module above 300 lines into a sibling file
(`#[cfg(test)] mod tests;` with `tests.rs` beside it). Two files are close
to 1,000 production lines on their own: `pathfinder/mod.rs` and
`world/mod.rs`. Do not add code to them before F2 and the scan merge below
shrink them.

## Duplication

### Two spiral block scans

`find_states` (`src/world/mod.rs:528`) and `find_matching_unlocked`
(`:608`) share the chunk-layer loop, the "stop one layer after reaching
`count`" exit, the closest-first sort, and the truncate. They differ only
in how one chunk produces matching positions. One function takes the
per-chunk step as a closure:

```rust
fn spiral_find(
    point: BlockPos,
    max_distance: f64,
    count: usize,
    mut chunk_matches: impl FnMut(ChunkPos, &mut Vec<BlockPos>),
) -> Vec<BlockPos> {
    let mut found = vec![];
    let layers = ((max_distance + 8.) / 16.).ceil().max(1.) as u32;
    let mut iter = ChunkIterator::new(ChunkPos::from(point), layers);
    let mut started_layer = 0;
    while let Some(chunk_pos) = iter.next() {
        chunk_matches(chunk_pos, &mut found);
        if started_layer != iter.layer && found.len() >= count {
            break;
        }
        started_layer = iter.layer;
    }
    found.sort_by(|a, b| a.distance_to(point).total_cmp(&b.distance_to(point)));
    found.truncate(count);
    found
}
```

`euclidean` (`:503`) is an identity wrapper over `BlockPos::distance_to`
and goes away with it. `Bot::find_blocks` fills the default `point`, and
`WorldRef::find_blocks` fills it again; keep the second.

### Three eye positions and three raycasts

`Bot::eye_pos` and `Bot::eye_and_look` (`src/world/mod.rs:943`, `:949`)
and `Bot::dig_eye_pos` (`src/interact/dig.rs:565`) run the same
`try_query_self::<(&Position, &EntityDimensions, ..)>`. `raycast_blocks`
and `raycast_unlocked` (`src/world/mod.rs:649`, `:690`) and
`Bot::raycast_hit` (`src/interact/dig.rs:574`) are three wrappers over
`traverse_blocks` with the same zero-length and range guards. Keep one
eye helper and one raycast with a visitor argument in `world`.

### Two "best tool" scorers

`Pathfinder::best_harvest_tool` (`src/pathfinder/mod.rs:638`) and
`Tool::best_tool_for` (`src/tool.rs:269`, over `dig_progress` at `:107`)
both scan the player slots and maximize `get_mine_progress`. They differ
in the bare-hand candidate and the tie rule. `bot.dig_time`
(`src/interact/dig.rs:452`) is a third caller of the same query. One
`mine_progress(&self, stack, block) -> f32` on `Bot` plus one argmax
serves all three; the tie rule becomes a parameter, or one caller adopts
the other's rule if upstream's two plugins agree.

### Smaller copies and wrappers

- `facade_context` and `self_position` (`src/pathfinder/tracking.rs`)
  repeat `with_component` and `Client::position`.
- `open_container` (`src/inventory/mod.rs:753`) inlines the body of
  `open_block` for the block case.
- `same_goal` (`src/pathfinder/goal.rs:178`) is a hand-written structural
  equality for `Goal`. A `PartialEq` derive works once `Goal::Follow`
  compares entities by id (one manual impl on a newtype, or a custom
  `PartialEq` for that variant only).
- `SharedGoal` (`goal.rs:231`) exists because the engine's
  `start_goto_with_opts` takes `impl Goal`, not `Arc<dyn Goal>`. A
  blanket `impl<G: Goal + ?Sized> Goal for Arc<G>` in the engine deletes
  the wrapper. `AnyGoal`, `AllGoal`, `InvertedGoal`, `GetToBlockGoal`, and
  `NearXZGoal` are generic goals; they belong in the engine's `goals`
  module beside `OrGoals` and `AndGoals`.
- `GetToBlockGoal::heuristic` and `NearXZGoal::heuristic` each compute
  the octagonal xz distance by hand.
- `pvp::rand_next` is a hand-written xorshift; the engine depends on
  `rand` already.
- `craft_once` writes `if window_table.is_none() { … } let window =
  window_table.clone().unwrap();`; `get_or_insert` with an async open is
  clearer, or a `match`.
- `is_container_block_name` (`src/inventory/mod.rs:136`) matches block
  name strings; a `BlockKind` set (or a block tag) is typed.

## Locks and copies in hot paths

- `misc::tick_diffs` (`src/misc/mod.rs:92`) takes `client.ecs.write()`
  every game tick to update `was_sleeping` and `last_air`, which are port
  baselines. Keep the baselines in bridge-local state and read the ECS
  with a shared lock, or (with F1) use Bevy change detection
  (`Changed<Pose>`, `Changed<AirSupply>`).
- `handle_packet` (`:131`) builds `&mut MiscState::default()` for nine of
  ten packet arms so that it can call a reducer that wants state. Split
  the one stateful arm (`ResourcePackPush`) from the stateless decode.
- `Window::slot` (`src/windows/mod.rs`) calls `menu()`, which clones the
  whole `Menu`, to read one slot; its doc says it is the cheap path.
  Read the slot under the component guard.
- `Block::behavior`, `property`, and `properties` each allocate a
  `Box<dyn BlockTrait>` per call.
- `with_world` logs `warn!` on every read after teardown.

## Text that no longer matches the code

| Place | Problem |
|---|---|
| `src/windows/mod.rs:30-41` (module header) | says the observer "patches" `carried_item` and `c_set_player_inventory`; plan item A3 moved both into the engine |
| `src/windows/mod.rs:437-441` (`menu_for_id`) | "the id-0 packet writes our observer patches into `inventory_menu`"; the observer no longer writes |
| `src/interact/mod.rs:19-20` | names the engine `Mining` component as the `targetDigBlock` source of truth; `dig.rs` owns `dig_target` |
| `src/pathfinder/mod.rs:795-796` | an orphan first sentence of the deleted `stop_pathfinding_patched` doc, glued to the doc of `follow_driver` |
| `crates/mineflayer-engine/src/events.rs:151-153` (`PathEvent::PathFound`) | says an empty path with `is_partial == false` means no route; the A* returns an empty *partial* path for no route, which the port's classifier already assumes |
| 13 places in `src/` and `docs/api-parity.md` | rename leftovers: "no the engine analog", "; The engine uses" |
| 33 comments in `src/` | cite plan IDs (`N-3`, `M-4`, `M-7`, `U9`, `U12`, `(first plan B8)`, …) instead of stating the rule; the IDs point into replaced plans |
| `src/window_types/mod.rs:266` | an empty `impl Bot {}` |

## Public surface that does nothing

`Movements` (`src/pathfinder/movements.rs`) has ten public fields. Six
(`can_open_doors`, `allow_sprinting`, `allow_free_motion`,
`dont_create_flow`, `dont_mine_under_falling_block`, `max_drop_down`) are
documented as "Unhonored": the engine ignores them. A caller who sets
`max_drop_down = 1` to keep a bot off cliffs gets no effect and no error.
Either implement them in the successors function (F8) or remove them
until they work and record the removal in the parity table. Accepting
and ignoring safety-relevant config is the worse choice.

`MovementConstraints` reaches the successors function through the
engine's `CustomPathfinderState` typemap because `SuccessorsFn` is a plain
`fn` that cannot capture. `sync_constraints` then needs a lock-order
comment to publish them. If the engine's `PathfinderOpts` carried an
`Arc<dyn Fn(&mut MovesCtx, RelBlockPos)>` (or a constraints field), the
typemap, `sync_constraints`, and the lock comment would go away. This is
an engine change; weigh it against the merge cost (D-engine).
