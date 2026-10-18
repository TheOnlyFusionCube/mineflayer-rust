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

**D-locks (first plan B8). Every crate lock is `parking_lot`.** The engine's ECS lock is
already `parking_lot`, and the type has no poisoning, so one panic under a
lock cannot break a domain for the rest of the session.

**D-lock-order (first plan B9). A domain lock may be held while the ECS lock is
taken; never the reverse.** The rule and every two-lock site are documented
next to `BotShared` (`src/bot/mod.rs`). Plan item A3 (2026-10-17)
removed the window tracker's ECS writes, but it still reads the ECS under
its lock, as does the world-state observer, so the rule stays as it is.

**D-log (first plan C1). `tracing` is the log facade.** The engine already logs through
it. The crate emits and never installs a subscriber; it never logs chat
text, tokens, or account data. `tracing-subscriber` is a dev-dependency
for the examples.

**D-storage (first plan D2). Where per-bot state lives.** ECS component when an ECS
system or an ECS query with engine components reads it; a component created
on first use for plugin-analog state; `BotShared` for everything else. See
`BotShared`.

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
domain function with the inputs that domain needs, so the domain
functions keep narrow arguments and their unit tests.

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
  would only change where the packet arrives from.

Cost if wrong: add the engine event, record it in the D-engine list, and
replace the port decode with a match on it.

**D-events. One `BotEvent` enum.** Events are one broadcast protocol;
nested event types would cause match churn with no gain.

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
