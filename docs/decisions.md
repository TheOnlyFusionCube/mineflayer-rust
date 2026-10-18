# Decision record

The rulings that the code depends on. The original working notes are in
`.superpowers/`, which git ignores; this file is the tracked copy.
Each entry gives the decision, the reason, and the cost if it is wrong.

## Dependencies and platform

**D-engine. The client engine is in this repository** (`crates/`, 21
`mineflayer-*` crates, 2026-10-16). It is a renamed copy of the upstream
engine at commit `ddef371`, the revision the port was pinned to before
(provenance and license text: `LICENSE`, "Third-party notices"). This
supersedes D-pin and the PORT-PLAN rule "never vendored". Reasons: the port
needed an unpublished upstream fix (`ServerboundAttack` writes its entity id
as a VarInt; the published 0.16.0 wrote a fixed i32 and each `bot.attack()`
caused a kick), dependent crates had to repeat a 21-line `[patch]` table,
and the port must not carry third-party branding. The renames: crates
`azalea*` → `mineflayer-*`, the facade `azalea` → `mineflayer-engine`
(re-exported as `mineflayer::engine`), the derive and traits `AzBuf*` →
`McBuf*`, and the methods `azalea_read*`/`azalea_write*` →
`mc_read*`/`mc_write*`. Game data with the word (the azalea blocks, their
sounds, and tags) is not renamed. Cost: the port owns about 123,000 lines
and moves to a new Minecraft version by a manual merge
(`docs/version_upgrade.md`). Cost if wrong: replace the path dependency
with a registry dependency and reverse the renames in `src/`.

Local engine changes after the copy (examine each one after an upstream
merge; plan item E2 keeps this list current):

1. Token renames by `scripts/vendor_engine.py`, then a prose pass over
   comments, build-script messages, and test identifiers (`azalea:` test
   namespaces became `mineflayer:`; the internal serde marker names
   `azalea:int_array`/`azalea:long_array` became `mineflayer:…`).
2. `cargo fmt --all` with `crates/rustfmt.toml`, and `cargo clippy --fix`
   (five fixes plus `FixedBitSet::mc_write` iterating bytes).
3. The `McBuf` derive emits field shorthand (`mineflayer-buf-macros`).
4. Three attribute-only doctests in `mineflayer-core/src/codec_utils.rs`
   are `ignore`.
5. `use core::f32;` removed from the pathfinder world module.
6. The `_docs` module of `mineflayer-engine` removed.
7. `log` is not a default feature of `mineflayer-engine` or
   `mineflayer-client` (commit `7a2bc95`).
8. User-visible names: the Microsoft token cache is
   `mineflayer-auth.json`; the packet-log env var is
   `MINEFLAYER_DO_NOT_CUT_OFF_PACKET_LOGS`.
9. (A2) `handle_stop_pathfinding_event` clears `goal` and
   `is_calculating` on every stop, also when the entity has no
   `ExecutingPath` (upstream left `is_calculating` set and skipped the
   clear entirely).
10. (A1) `Event::Pathfinder(PathEvent)` carries pathfinder results
    (path/is_partial/elapsed, goal reached, stopped) on the client event
    stream, and `PathFoundEvent` gains `elapsed`; the port translates
    these instead of diffing engine components per tick.
11. (A3) The inventory packet handlers apply what the port used to patch
    in from outside: `container_set_content` on the active menu sets
    `carried` and `state_id` (vanilla `initializeContents`); `set_slot`
    on the active menu adopts `state_id`, hotbar writes included; every
    window-0 `set_slot` reaches the player menu (vanilla drops non-hotbar
    ones while a container is open; upstream mineflayer applies them);
    `set_player_inventory` and `set_cursor_item` are applied (upstream
    ignored both); content before `open_screen` is buffered in
    `Inventory::early_content`; and syncs under a client-closed id reach
    the player menu through `Inventory::last_closed` (vanilla drops them;
    upstream mineflayer's `lastClosedWindow`). `Player::slot_for_inventory_index`
    maps `set_player_inventory` slots. Tests:
    `mineflayer-client/tests/simulation/inventory_packets.rs`.
12. (A4) `ClientPlugins` (the default plugin groups) and
    `StartClientOpts::new_with_plugins` let a caller add and disable
    plugins without a copy of the bootstrap.
13. (A5) `CancelMiningEvent` (an entity event, mining plugin) drops a
    queued start, also a `StartMiningBlockEvent` written before it and
    not read yet, and aborts live mining. The start handler skips start
    messages with an id below the entity's `MiningCancelledBefore`; starts
    written after the cancel are not affected. Tests:
    `mineflayer-client/tests/simulation/cancel_mining.rs`.
14. (D1) `mineflayer_registry::tags::item_tag_by_name` maps a tag name to
    its static in the generated `tags/items.rs`. It is written by hand in
    `tags/mod.rs`, so the generated file stays as upstream makes it. Its
    test reads the statics out of `items.rs` and fails when a tag has no
    entry, so add the new names after a merge that adds tags.
15. (E4) Removed modules the port and the engine tests do not use: `swarm`
    (with `SwarmBuilder`, the swarm plugins, and its prelude), `builder`
    (`ClientBuilder`, which wrapped `SwarmBuilder`), `join_opts`
    (`JoinOpts`), `auto_reconnect`, and the `NoState`/`HandleFn` handler
    types. `ClientPlugins` is `DefaultPlugins` and `DefaultBotPlugins`. An
    upstream merge drops its changes to these files.
16. (DF3) `Event`, `PathEvent`, `GotoEvent`, `PathfinderOpts`,
    `Pathfinder`, and `PathFoundEvent` are not `#[non_exhaustive]`. The
    engine is not published, so the attribute only hid new variants from
    the port's matches; now a new variant fails to compile in the bridge.
    An upstream merge that adds the attribute back must drop it again.
17. (DF5) `calculate_path` does not sleep after a partial result; the
    goto task sleeps 100 ms instead, so the instant-path try, the
    obstruction patch, and the port's `get_path_from_to` do not stall
    their caller. A path that cannot be replayed over the search's world
    cache logs an error and returns `None` instead of panicking.
18. (DF2) A game packet that fails to decode writes the
    `GamePacketDecodeFailedEvent` message, which becomes
    `Event::PacketDecodeFailed`; the error log stays. Test utilities:
    `Simulation::receive_raw_packet` and `init_allowing_errors`. Test:
    `mineflayer-client/tests/simulation/game_packet_decode_failed.rs`.
19. (DF1) `FormattedText::Unresolved` holds keybind, selector, score,
    nbt, and object components (`mineflayer-chat/src/unresolved_component.rs`):
    the kind, the style and `extra` children, and the other fields as
    JSON, written back unchanged. Display is vanilla's fallback. Upstream
    returns an error for these kinds, which dropped the whole packet.
20. (DF4) `mineflayer_auth::DeviceCodeCallback` receives the Microsoft
    device code. `AuthOpts::on_device_code`,
    `MicrosoftAccountOpts::on_device_code`, and a fifth argument of
    `interactive_get_ms_auth_token` carry it; `None` keeps upstream's
    `println!`.
21. (TS1) `test_utils` builds with the new `test-utils` feature of
    `mineflayer-client` (and `mineflayer-engine`), not only with `log`;
    its `tracing` module still needs `log`. `Simulation::new_with_plugins`
    builds the app from any plugin group. The port turns `test-utils` on
    in its dev-dependencies for `BotSim` (`src/test_util/sim.rs`).
22. (TS2) The `EventsPlugin` listeners run chained: `Packet`, then
    `PacketDecodeFailed`, `Login`, `ReceiveChunk`, `Chat`, the player-list
    events, `Spawn`, `Death`, `Disconnect`, `ConnectionFailed`. Upstream
    registers them as one unordered tuple, so the events of one pass
    went out in an order that changed from run to run (found by the
    golden test, `tests/golden/bridge_events.txt`).
23. (ST2) `connection::PacketPreApply`, a resource of
    `fn(&mut World, Entity, &ClientboundGamePacket)` that
    `handle_raw_packet` calls before `process_packet` (through
    `resource_scope`, no allocation per packet). `PacketPlugin` inits it
    empty. Test: `mineflayer-client/tests/simulation/packet_pre_apply.rs`.
24. (ST4) The `login` and `respawn` game handlers clear
    `Inventory::early_content` and `Inventory::last_closed`, as upstream
    mineflayer drops `windowItems` and `lastClosedWindow`. The port's
    window tracker used to clear its own copies; the engine kept stale
    ones. Test: `login_and_respawn_drop_early_content_and_the_last_closed_container`
    in `mineflayer-client/tests/simulation/inventory_packets.rs`.
25. (OP2) Goto identifiers. `pathfinder::GotoId` is on `GotoEvent` (a
    repath keeps its goto's id) and on every `PathEvent`;
    `start_goto`/`start_goto_with_opts` return it, and `Pathfinder::goto`
    holds the current one. `PathfinderOpts::total_timeout` bounds the
    search time of one goto's partial searches; when it is used,
    `PathEvent::Timeout` goes out once and the goal stays (upstream
    `thinkTimeout`). Test: `a_goto_over_its_total_time_times_out_once_and_keeps_its_goal`
    in `mineflayer-engine/src/pathfinder/tests.rs`.
26. (CP7) `events::login_listener` sends `Event::Login` for each
    `ClientboundLogin` packet; it fired once per entity before
    (`Added<MinecraftEntityId>`), so a proxy's second login gave no
    `login`. Test: `each_login_packet_gives_login` in the port's
    `src/test_util/sim.rs` (it runs the real `EventsPlugin`).
27. (CP5) `Event::ResourcePack(ResourcePackOffer)`, sent by the chained
    `resource_pack_listener` from the `ResourcePackEvent` message of both
    phases, with `in_config`. The port's misc decode of the game-phase
    `resource_pack_push` packet is gone. Test:
    `a_denial_is_the_only_answer_when_accept_is_off` in the port's
    `src/plugins/tests.rs`.
28. (OP3) `PathEvent::Timeout` carries the partial path of the search
    that used the last of the time, so the port's `path_update` with
    status `timeout` has upstream's `results.path`. Test:
    `a_goto_over_its_total_time_times_out_once_and_keeps_its_goal`.
29. (OP5) Mining identifiers. `mining::MiningId` is on
    `StartMiningBlockEvent`, `MiningQueued`, `Mining`, and
    `FinishMiningBlockEvent`. Each start ends in one
    `MiningFinished { entity, id, outcome }` message: `Broken` when the
    client predicts the break, `Cancelled` for a stop, a cancel, or a
    newer start, and `Interrupted` for the other exits (a restricted
    interaction, a lost look, a block that was already gone). Tests:
    `mineflayer-client/tests/simulation/mining_finished.rs`.
30. (CL4) `pathfinder::goals` has `GetToBlockGoal` and `NearXZGoal`
    (mineflayer-pathfinder goals the port used to define), and
    `impl<G: Goal + ?Sized> Goal for Arc<G>`, so `Arc<dyn Goal>` goes into
    `OrGoals`, `AndGoals`, and `InverseGoal`. `EntityRef` is `PartialEq`
    and `Eq` by entity. Tests: `port_goal_tests` in
    `mineflayer-engine/src/pathfinder/goals.rs`.
31. (CP2) `PathfinderOpts::allow_sprint` and `ExecuteCtx::allow_sprint`:
    with `false`, `ExecuteCtx::sprint` walks. Test:
    `a_goto_without_sprint_walks` in
    `mineflayer-engine/src/pathfinder/tests.rs`.

**D-codegen. The code generator stays upstream-only** (plan item E1,
2026-10-16). The repository does not contain `codegen/`. The generated
files (registry, blocks, item components, entity metadata, language,
packets) are Python output from the upstream project. At each upgrade they
arrive with the renamed upstream diff (`docs/version_upgrade.md` §1), and
they stay byte-identical to upstream apart from the D-engine rename table.
Local changes inside a generated file belong to the list above and are
examined again after each merge (plan item E2). Reasons: no second copy of
the generator to keep in step, and no drift between the local output and
the upstream output. Cost if wrong: copy `codegen/` into the repository,
change the scripts to write the `mineflayer-*` names, and regenerate.

**D-publish. Nothing is published to crates.io** (plan item E3,
2026-10-17). The port has no public remote and no release process that
publishes, and publishing `mineflayer` would first need its 21 path
dependencies published. Every engine crate has `publish = false`
(`[workspace.package]` in `Cargo.toml`), so `cargo publish` refuses them,
and `mineflayer` cannot be published while it depends on them. Cost if
wrong: remove the flag, give the engine crates their own version line and
`repository`/`description` fields, and publish them first.

**D-toolchain. Nightly `2026-10-01` and `-Znext-solver=no`.** The engine
crates need nightly. The nightly current at scaffold time stopped with an
internal compiler error on the entity crate, and the next-gen trait solver
cannot infer `mineflayer-core`'s const generics. Review monthly (first plan, H2, at `775ba66`).

**DEC-1 (open, for the owner). Must the port and the engine build on
stable Rust?** Spike (plan item AD0, 2026-10-17): `cargo +stable check
--workspace --all-targets` with stable 1.98.1, `RUSTFLAGS` cleared, and
`--ignore-rust-version` (the crate declares `rust-version = 1.101`).
Results:

1. `simdnbt 0.10.0`, the newest release, does not build on stable: it
   uses `#![feature(portable_simd)]` (`swap_endianness.rs`, `mutf8.rs`)
   and `#![feature(allocator_api)]` (`FastVec<T, A: Allocator>` in
   `fastvec.rs`). Seven engine crates depend on it, so the check stops
   there and cannot reach the engine's own gates. This blocker is outside
   the seven local gates that the audit listed.
2. `mineflayer-engine/build.rs` panics on any compiler that is not
   nightly (`check_nightly`).
3. Not yet seen, because of item 1: the local gates
   (`generic_const_exprs` in core, `type_changing_struct_update` in
   engine, `min_specialization` in inventory and buf, `trait_alias` in
   physics, `error_generic_member_access` in buf, protocol, and client)
   and the `-Znext-solver=no` need.

Estimate for AD1: the 3 HD of the plan for the local gates and the build
script, plus 1.5 to 2 HD for a stable `simdnbt` (a fork in `crates/` or a
`[patch]`: scalar code in place of `std::simd`, and `FastVec` without the
allocator parameter), plus its merge cost, because each `simdnbt` release
must be ported again. Next step if the owner says yes: patch `simdnbt`
locally and rerun the check to list the remaining errors.

**D-locks (first plan B8). Every crate lock is `parking_lot`.** The engine's ECS lock is
already `parking_lot`, and the type has no poisoning, so one panic under a
lock cannot break a domain for the rest of the session.

**D-lock-order (first plan B9). Removed** (third plan ST7, 2026-10-18).
The rule ordered the `BotShared` domain locks against the ECS lock. The
domain state is now in ECS components, so the port has one lock for game
state; `quit_reason` and `end_reason` are never held across it. What
stays is the ECS lock's own rule: it is not reentrant, so code that runs
under it (`with_component`, `with_component_mut`) must not lock it again.

**D-log (first plan C1). `tracing` is the log facade.** The engine already logs through
it. The crate emits and never installs a subscriber; it never logs chat
text, tokens, or account data. `tracing-subscriber` is a dev-dependency
for the examples.

**D-storage (first plan D2). Where per-bot state lives.** ECS component when an ECS
system or an ECS query with engine components reads it; a component created
on first use for plugin-analog state; a component on another entity for
state about that entity (third plan ST5: equipment, attributes, vehicle
links), so it despawns with the entity; `BotShared` only for what is not
game state. See `BotShared`.

## Architecture

**D-bootstrap. The port joins through the engine bootstrap.**
`connect` calls `StartClientOpts::new` (`src/bot/connection.rs`), the same
app `Client::join` builds. The port does not mirror the engine's plugin
list (plan item A4, 2026-10-17). A dig abort uses the engine's
`CancelMiningEvent` (A5), so the port adds no plugin, and the engine no
longer has the auto-reconnect and swarm plugins the port used to disable
(E4). `StartClientOpts::new_with_plugins` stays for a plugin the port
may need later.

**D-bridge. One ordered observer list.** Every domain observer runs inside
the bridge task, so derived events follow their source packet and a
lagging broadcast consumer cannot drop them. `OBSERVERS` in
`src/bridge.rs` is the order (plan items B1/B2, 2026-10-17): the direct
translation (`Packet`, `Tick`, lifecycle), misc, scoreboard, the
lifecycle and chat events derived from the packet, movement, pathfinder,
entity, windows, window types, craft, world. This is the order the bridge
loop had before; `bridge_emission_order` pins it, and upstream
`health.js` emits `health` before the re-arm `spawn` as it does. All
observers run before the first event of an engine event goes out. Each
entry is an adapter that passes one shared `Observe` context to the
domain function with the inputs that domain needs. The golden test of
TS2 (`tests/golden/bridge_events.txt`) pins the order for a full packet
script; `bridge_emission_order` pins the lifecycle and chat cases.

**D-port-plugins. The bridge stays the one sender; per-bot state moves into
the ECS** (third plan, ST1 and ST3, 2026-10-17). The third plan's ST1 put
every observer into a Bevy system so that the event order would be a
system order. The golden test of TS2 showed that the order problem came
from the engine's unordered `EventsPlugin` listeners (D-engine item 22),
and with those chained the bridge order is deterministic. Moving the
observers into the schedule would also deadlock: they read the ECS
through `Client`, whose lock the runner holds for the whole pass. So the
bridge keeps translating engine events between passes, in engine-event
order, and the port adds Bevy plugins (`src/plugins.rs`, `BotPlugins`,
built through `StartClientOpts::new_with_plugins`) only to record facts
the bridge cannot read after a pass. The first is the world plugin: it
records the state each queued block update replaces, before the engine
applies it, so `BlockUpdate.old` is right on the first update of a
position. Domain state the bridge writes lives in components on the
client entity (`WorldState` is one), not in `BotShared` locks. Cost if
wrong: a domain whose events must come from inside the pass needs a
port-side queue that the bridge drains in order.

**D-redecode. The port keeps its packet re-decodes** (plan item A6,
2026-10-17). For each place the port decodes what the engine drops or
keeps inside the ECS (`docs/reviews/01_engine_boundary.md`, B10 to B13),
an engine event would not make the port shorter by enough to pay for a
new engine change:

- Action bar, tab list, command suggestions (`bridge::packet_side_events`):
  one match arm each on `Event::Packet`; an engine event is one arm too.
- `move` (`movement::bridge_events`): a 15-line per-tick diff of
  `Position`/`LookDirection`, equal to the engine sending a position
  packet when either changed. An engine "position sent" event would
  remove the diff and `MovementTracker` and add the same amount of engine
  code. `forcedMove` and `abilities` are one arm each.
- Sleep, wake, breath (`misc::tick_diffs`): a per-tick diff of `Pose`
  and `AirSupply`; an engine event would move the same comparison into
  the engine.
- Equipment, chat-session keys, attributes (`entity::tracker`): the
  engine does not store them, so the port must keep the state; an event
  would only change where the packet arrives from. Since third plan ST5
  they are components on their entity, and the metadata events read the
  engine's typed metadata decode instead of protocol indices.
- Block entities (`world::block_entity`, third plan CP1): the engine
  stores none, so the port keeps them by position from the chunk and
  `block_entity_data` packets, in a component on the client entity.
- Where the engine's apply destroys what an event needs (a removed
  entity, an ended effect, a replaced slot, a replaced block), a port
  plugin records it before the apply: the `PacketPreApply` hooks of
  `entity::EntityStatePlugin` and `windows::WindowPlugin`, and the block
  system of `world::WorldPlugin` (D-port-plugins).

Cost if wrong: add the engine event, record it in the D-engine list, and
replace the port decode with a match on it.

**D-events. One `BotEvent` enum for the core.** The core events are one
broadcast protocol; nested event types would cause match churn with no
gain. A plugin's events are its own type on its own channel
(`Bot::plugin_events`), so a plugin crate needs no core variant (third
plan PL3).

**DEC-3 (decided 2026-10-18). The plugin analogs are in one
`mineflayer-plugins` crate, one cargo feature each, and the core does
not re-export them.** The plan's recommendation had the core re-export
the crate behind default features. That is a dependency cycle
(`mineflayer-plugins` depends on `mineflayer`), which cargo refuses. A
dependent adds the crate, as an upstream user adds
`mineflayer-pathfinder` or `mineflayer-pvp`. The core keeps a
dev-dependency on it for the live plugin test (cargo allows that
cycle). `pathfinder` stays in the core: `collect`, `pvp`, and `tool`
use it, and its follow and dynamic goals step inside the bridge.

**D-errors (first plan B1). Typed error variants keep upstream messages.**
`InvalidArgument`, `InvalidState`, `NotFound`, `Failed`, `Cancelled`,
`Unsupported`, and `DigAborted` replace `Operation(String)`; each keeps the
upstream text as `Display`.

**D-fixture (E5). Deterministic fixture spawns.** `scripts/live-server.sh`
sets `respawn_radius 0` over RCON after each boot. Vanilla's default radius
put each join at a random point in a 21×21 area, so the live tests depended
on random terrain (a walled-in spawn, no grass in range). RCON listens on
`127.0.0.1` only (`server-ip`), with a random password per start kept in
the git-ignored `.live/server.properties`. The live tests stay
terrain-tolerant as well; the fixed spawn removes the main source of
variance.

## Audit rulings (second-model audit campaign)

- **R1** Pin the engine to `ddef371` (superseded by D-engine above).
- **R2** Reject the entity audit finding. The claim was false: upstream
  `activateEntity` does look at position + 1 (`inventory.js:254`).
- **R3** Reject the craft audit. All three claims failed source checks; the
  response was also truncated.
- **R4** Reject the collect audit. prismarine-windows
  `firstEmptyContainerSlot` is `firstEmptySlotRange(0, inventoryStart)`,
  which the port already matches; the proposed method does not exist.
- **R5** Reject the tool audit. `Box::pin` is a Rust requirement, not a
  divergence.
- **R6** Reject the auto_eat audit. The rewrite was semantically identical,
  added a dead binding, and damaged a test.
- **R7** Accept the misc `clearInventory` concurrency finding (slots clear
  concurrently, like upstream's `Promise.all`, with `join_all` so every
  started slot settles). Reject its `fish()` "3D distance" claim: upstream
  builds `Vec3(packet.x, pos.y, packet.z)`, so the check stays horizontal.
