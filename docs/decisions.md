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
12. (A4) `ClientPlugins` (the three default plugin groups) and
    `StartClientOpts::new_with_plugins` let a caller add and disable
    plugins without a copy of the bootstrap; `swarm` exports
    `SwarmPlugin` and `SwarmChatPlugin` so that they can be disabled.
13. (A5) `CancelMiningEvent` (an entity event, mining plugin) drops a
    queued start, also a `StartMiningBlockEvent` written before it and
    not read yet, and aborts live mining. The start handler skips start
    messages with an id below the entity's `MiningCancelledBefore`; starts
    written after the cancel are not affected. Tests:
    `mineflayer-client/tests/simulation/cancel_mining.rs`.

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
`connect` calls `StartClientOpts::new_with_plugins` with `client_plugins()`
(`src/bot/connection.rs`): the engine's `ClientPlugins` without
auto-reconnect and the swarm plugins, plus the dig-abort reconciler, which
must run inside the tick that materializes a pending mining start. The
port does not mirror the engine's plugin list (plan item A4, 2026-10-17).
A schedule-level test in `src/bot/connection.rs` runs the production app.

**D-bridge. One ordered observer loop.** Every domain observer runs inside
the bridge task, in a fixed order, so derived events follow their source
packet and a lagging broadcast consumer cannot drop them.

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
