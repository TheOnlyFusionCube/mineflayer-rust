# 01. State ownership: the port's second model of the game

Finding F1 in [`audit_summary.md`](audit_summary.md). Baseline `ef06f74`.

## What the port keeps today

Per-bot state lives in three places.

| Place | Holder | Contents |
|---|---|---|
| Engine ECS | engine components | position, physics, inventory menus, mining, pathfinder, entity metadata, world chunks |
| Port ECS components | 7 `#[derive(Component)]` types | `MiscState`, `InteractShared`, `PathfinderFacade`, `AutoEatState`, `CollectState`, `PvpState`, `ToolState` (plus the engine's `CustomPathfinderState` typemap) |
| `BotShared` (`src/bot/mod.rs:78`) | 9 `Arc<Mutex/RwLock>` | `quit_reason`, `end_reason`, `ChatShared`, `WorldState`, `EntityTracker`, `WindowTracker`, `WindowDataTracker`, `RecipeBook`, `ScoreboardState`, plus the event sender |

`BotShared` is filled by `bridge::spawn`, a tokio task that receives the
engine's `Event` stream over an unbounded channel and runs eleven
observers per event (`OBSERVERS`, `src/bridge.rs:220`). Each observer
pattern-matches `Event::Packet` and decodes the raw packet again.

The engine applies a game packet synchronously in `process_packet` and
only then queues the `ReceiveGamePacketEvent` that becomes `Event::Packet`
(`crates/mineflayer-client/src/plugins/connection.rs:275-278`). So every
port observer sees the world after the packet changed it. Most of the
port's hardest bookkeeping exists to recover what that order hides.

## Evidence

### The window state machine exists twice

Command: `grep -rn "early_content\|last_closed" crates/mineflayer-entity/src crates/mineflayer-client/src`.

| Engine (`crates/mineflayer-entity/src/inventory.rs`) | Port (`src/windows/tracker.rs`) |
|---|---|
| `early_content: Option<(i32, Vec<ItemStack>)>` (line 68) | `early_items: Option<(i32, Vec<ItemStack>)>` |
| `last_closed: Option<(i32, usize, usize)>` (line 72) | `last_closed: Option<(i32, usize, usize)>` |
| active menu and `container_menu` | `pending: Option<PendingWindow>`, `current: Option<Arc<WindowShared>>` |
| the menu slots | `slots: HashMap<i32, Vec<ItemStack>>` (the `old` side of `WindowUpdateSlot`) |

Plan item A3 moved the window packet semantics into the engine handlers.
The port kept its own copy so that it can produce `WindowUpdateSlot.old`,
`windowOpen` timing, and `setWindowItems`. The two copies now implement
the same rules (`open_screen` consumes early content, a close seeds
`last_closed`, a login clears both), and nothing checks that they agree.
`set_content_events` (82 lines) and `observe_event` (161 lines, the
longest function in the port) are the port copy.

`Window::close` (`src/windows/mod.rs:373`) still takes
`client.ecs.write()` and copies the container's player region into
`inventory_menu` (upstream `copyInventory`). That is an engine write from
the port, the class of write plan item A3 was meant to remove.

### Block updates lose the old state

`WorldState::last_block_updates` (`src/world/tracker.rs:45`) keeps up to
`MAX_TRACKED_UPDATES = 1 << 16` block states, clears on login, respawn,
and chunk unload, and clears wholesale when full. Its field doc explains
why: by the time the bridge reads `Event::Packet`, the engine's
`handle_block_update_event` has already applied the update. For a
position never updated before, the port falls back to the post-apply
read, so `BotEvent::BlockUpdate` reports `old == new`. Upstream reads
`oldBlock` before it writes, so it is always correct. The parity table
lists this as `diverges` (`docs/api-parity.md:154`).

Block updates are queued (`QueuedServerBlockUpdates`,
`crates/mineflayer-client/src/plugins/block_update.rs:30`) and applied by
a system. A port system ordered before that system reads the true old
state with no engine change.

### Entities are tracked in nine parallel maps

`EntityTracker` (`src/entity/tracker.rs:23-58`) keeps `known`, `meta`,
`equipment`, `attributes`, `effects`, `passengers_of`, `vehicle_of`,
`seen_fireworks`, and `chat_sessions`, all keyed by `MinecraftEntityId`
or `Uuid`, beside the ECS entities they describe. `forget` and
`forget_other_entities` must remember every map. `known` exists because
the engine despawns an entity before the port sees `RemoveEntities`, so
`resolve` falls back to an id remembered from earlier packets.

`metadata_update` (99 lines) reads metadata by raw index and bit mask:
index `6` is the pose, `14` is the sleeping position, `0` is the shared
flags with `0x80` for elytra and `0x02` for sneaking. The engine already
decodes these into typed components (`FallFlying` and
`AbstractEntityShiftKeyDown` at `crates/mineflayer-entity/src/metadata.rs:233-248`,
`SleepingPos` at line 2843). The raw indices change between Minecraft
versions; the typed components come from the generator.

The engine drops `set_equipment` with a `debug!` only
(`crates/mineflayer-client/src/plugins/packet/game/mod.rs:1082`). It also
drops `set_entity_link`, `change_difficulty`,
`set_default_spawn_position`, `custom_payload`, `block_event`,
`set_player_team`, `animate`, `level_particles`, and others (22 handlers
log only; measured with an awk scan of that file). The port re-decodes
each of these.

### Two lock families hold the same facts

The lock-order rule (`src/bot/mod.rs:174-186`, D-lock-order in
`docs/decisions.md`) says that a domain lock may be held while the ECS
lock is taken, and never the reverse. Sixteen production comments carry
a `lock rule:` note (`grep -rn "lock rule" src`). The rule is needed only because the port's facts
live outside the ECS while the port reads ECS facts to compute them. If
the port state lives in the ECS, there is one lock and no rule.

## Why D-redecode does not settle this

D-redecode (`docs/decisions.md`, plan item A6) rejected engine events for
the re-decodes because each engine event would cost as much engine code as
it saves port code, and each engine change adds merge debt. That argument
compares a port decode with an engine decode. It does not consider the
third option: the port's decode runs as a Bevy system that the port
owns, added through the engine's existing plugin hook. That option adds
no code to the engine (apart from one hook, below), keeps the merge
surface as it is, and still removes the second state model.

## Proposal: a port-owned plugin group

```rust
// src/plugins/mod.rs (new): the port's ECS side.
pub(crate) struct PortPlugins;

impl PluginGroup for PortPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(WorldStatePlugin)     // time, weather, spawn point, block updates
            .add(WindowEventsPlugin)   // windowOpen/close, updateSlot, setWindowItems
            .add(EntityStatePlugin)    // equipment, attributes, effects, vehicles
            .add(ScoreboardPlugin)     // objectives, teams, boss bars
            .add(ChatStatePlugin)      // tablist, tab-complete replies, patterns
            .add(MiscStatePlugin)      // titles, sounds, particles, resource packs
            .add(BotEventPlugin)       // drains BotEventQueue into the broadcast
    }
}
```

1. `connect_engine` calls `StartClientOpts::new_with_plugins` with
   `PortPlugins` (`crates/mineflayer-engine/src/client_impl/mod.rs:114`
   already has it; the port does not use it).
2. Each domain plugin has a system that reads `ReceiveGamePacketEvent`
   and writes components on the affected entity (for example an
   `Equipment` component on the entity that `set_equipment` names) or a
   resource on the client entity (for example `WorldTime`, `Weather`,
   `SpawnPoint`, `Scoreboard`).
3. Each system pushes the `BotEvent`s it derives into one
   `BotEventQueue` component in the order it runs. A final system in
   `PortPlugins` drains the queue into the broadcast sender. The
   emission order that `bridge_emission_order` pins becomes a system
   order (`.chain()`), which Bevy checks.
4. For packets the engine applies synchronously and destroys
   information (removal of entities, close of a window), the engine gets
   one generic hook: a `PacketPreApply` resource holding a list of
   `fn(&mut World, Entity, &ClientboundGamePacket)` that
   `handle_raw_packet` calls before `process_packet`. This is one engine
   change of about 15 lines, recorded in the D-engine list. Block updates
   need no hook, because their apply is a system and ordering suffices.
5. `Bot` methods read components through `Client::get_component` and the
   existing `with_component` helpers. There are no domain locks.

## What goes away

| Removed | Where |
|---|---|
| `WindowTracker.pending`, `early_items`, `last_closed`, `slots` | `src/windows/tracker.rs` (the engine's copy is the only one) |
| `last_block_updates`, `MAX_TRACKED_UPDATES`, the chunk-unload retain | `src/world/tracker.rs` |
| `EntityTracker` maps and `forget*` | `src/entity/tracker.rs` (components despawn with their entity) |
| raw metadata index decode | `src/entity/tracker.rs:480-579` |
| 7 of the 9 `BotShared` locks | `src/bot/mod.rs` |
| the lock-order rule and its 16 `lock rule:` comments | `src/bot/mod.rs:174-186`, D-lock-order |
| most of `src/bridge.rs` (the observer list becomes system order) | `src/bridge.rs` |
| the unbounded engine `Event` channel as the port's input | `src/bot/connection.rs:334` |

What stays: the broadcast sender, `quit_reason` (set from `Bot::quit`
before the engine reports the disconnect), and the lifecycle translation
of `Event::Init`, `Login`, `Disconnect`, and `ConnectionFailed`.

## Risks and how to control them

The event order is public behavior (D-bridge). Write the simulation
harness of F9 first, record the event sequences of the current bridge for
a fixed packet script, and require the plugin version to produce the same
sequences. Migrate one domain per commit series.

ECS systems run while the engine holds the world. A port system must not
block. None of the current observers block; they only decode and push.

The engine's `Update` pass and the bridge task are on the same thread
today (the `LocalSet` in `connect`). Moving the observers into the
schedule removes one async hop per packet, so event latency goes down,
not up. Measure with `examples/event_rate.rs` before and after.
