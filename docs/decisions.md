# Decision record

The rulings that the code depends on. The original working notes are in
`.superpowers/`, which git ignores; this file is the tracked copy.
Each entry gives the decision, the reason, and the cost if it is wrong.

## Dependencies and platform

**D-pin. azalea is pinned to git commit `ddef371`** (`Cargo.toml`,
`[patch.crates-io]`, all 21 azalea crates). The published azalea 0.16.0
writes the `ServerboundAttack` entity id as a fixed i32. A release MC 26.1
server reads a VarInt, so each `bot.attack()` caused a kick ("larger than I
expected"). Commit `ddef371` ("fix attack packet was serialized incorrectly
(#323)") is on the same `0.16.0+mc26.1` line and is not published. The rule
"pinned git rev only if a needed API is unpublished" was applied to an
unpublished bug fix; vendoring is prohibited. All 21 crates are patched so
that the graph never mixes registry and git copies. Cost if wrong: remove
the patch table, resolve again, rebuild. Remove it when a release contains
the fix (plan item H1). Dependent crates must repeat the table (README,
"Use as a dependency").

**D-toolchain. Nightly `2026-10-01` and `-Znext-solver=no`.** azalea needs
nightly. The nightly current at scaffold time stopped with an internal
compiler error on `azalea-entity`, and the next-gen trait solver cannot
infer azalea-core's const generics. Review monthly (plan item H2).

**D-locks (B8). Every crate lock is `parking_lot`.** azalea's ECS lock is
already `parking_lot`, and the type has no poisoning, so one panic under a
lock cannot break a domain for the rest of the session.

**D-lock-order (B9). A domain lock may be held while the ECS lock is
taken; never the reverse.** The rule and every two-lock site are documented
next to `BotShared` (`src/bot/mod.rs`).

**D-log (C1). `tracing` is the log facade.** azalea already logs through
it. The crate emits and never installs a subscriber; it never logs chat
text, tokens, or account data. `tracing-subscriber` is a dev-dependency
for the examples.

**D-storage (D2). Where per-bot state lives.** ECS component when an ECS
system or an ECS query with azalea components reads it; a component created
on first use for plugin-analog state; `BotShared` for everything else. See
`BotShared`.

## Architecture

**D-bootstrap. `client_app::app()` mirrors azalea's `StartClientOpts::new`.**
`Client::join` builds the app with no plugin hook, and the dig-abort
reconciler must run inside the tick that materializes a pending mining
start. A schedule-level test in `src/client_app.rs` runs the production
app. Re-check the mirror when the azalea pin moves (plan item H4).

**D-bridge. One ordered observer loop.** Every domain observer runs inside
the bridge task, in a fixed order, so derived events follow their source
packet and a lagging broadcast consumer cannot drop them.

**D-events. One `BotEvent` enum.** Events are one broadcast protocol;
nested event types would cause match churn with no gain.

**D-errors (B1). Typed error variants keep upstream messages.**
`InvalidArgument`, `InvalidState`, `NotFound`, `Failed`, `Cancelled`,
`Unsupported`, and `DigAborted` replace `Operation(String)`; each keeps the
upstream text as `Display`.

## Audit rulings (second-model audit campaign)

- **R1** Pin azalea to `ddef371` (D-pin above).
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
