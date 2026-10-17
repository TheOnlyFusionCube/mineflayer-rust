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

**D-toolchain. Nightly `2026-10-01` and `-Znext-solver=no`.** The engine
crates need nightly. The nightly current at scaffold time stopped with an
internal compiler error on the entity crate, and the next-gen trait solver
cannot infer `mineflayer-core`'s const generics. Review monthly (plan item H2).

**D-locks (B8). Every crate lock is `parking_lot`.** The engine's ECS lock is
already `parking_lot`, and the type has no poisoning, so one panic under a
lock cannot break a domain for the rest of the session.

**D-lock-order (B9). A domain lock may be held while the ECS lock is
taken; never the reverse.** The rule and every two-lock site are documented
next to `BotShared` (`src/bot/mod.rs`).

**D-log (C1). `tracing` is the log facade.** The engine already logs through
it. The crate emits and never installs a subscriber; it never logs chat
text, tokens, or account data. `tracing-subscriber` is a dev-dependency
for the examples.

**D-storage (D2). Where per-bot state lives.** ECS component when an ECS
system or an ECS query with engine components reads it; a component created
on first use for plugin-analog state; `BotShared` for everything else. See
`BotShared`.

## Architecture

**D-bootstrap. `client_app::app()` mirrors the engine's `StartClientOpts::new`.**
`Client::join` builds the app with no plugin hook, and the dig-abort
reconciler must run inside the tick that materializes a pending mining
start. A schedule-level test in `src/client_app.rs` runs the production
app. Re-check the mirror when the engine changes (`docs/version_upgrade.md`).

**D-bridge. One ordered observer loop.** Every domain observer runs inside
the bridge task, in a fixed order, so derived events follow their source
packet and a lagging broadcast consumer cannot drop them.

**D-events. One `BotEvent` enum.** Events are one broadcast protocol;
nested event types would cause match churn with no gain.

**D-errors (B1). Typed error variants keep upstream messages.**
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
