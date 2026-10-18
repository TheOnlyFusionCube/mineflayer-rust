# 07. Tests, docs, and tooling

Finding F9 in [`audit_summary.md`](audit_summary.md), plus maintenance
notes. Baseline `ef06f74`.

## Measurements

| Item | Value | Command |
|---|---|---|
| Port unit tests | 254 pass, 0.35 s | `cargo test -p mineflayer --lib` |
| Port line coverage | 68.32% (measured at `2254020`) | `docs/coverage.md` |
| Lowest coverage | `window_types/anvil.rs` 5%, `furnace.rs` 20%, `villager.rs` 32%, `enchant.rs` 34%, `book.rs` 36%, `inventory/transfer.rs` 37% | `docs/coverage.md` |
| Live tests | 14 in one binary; CI runs them daily and on dispatch, not on push | `.github/workflows/verify.yml` |
| Engine simulation tests | 26 files in `crates/mineflayer-client/tests/simulation/` | `ls` |
| Port tests that use `Simulation` | 0 (`src/bridge.rs` names it in a comment only) | `grep -rln Simulation src tests` |
| `target/` | 42 GB | `du -sh target` |

## The test gap

The port's unit tests run over one of three scaffold clients from
`src/lib.rs` `test_util`: a client whose entity is already despawned, a
client with a component bundle and no systems, and a client with a tick
broadcaster and no systems. Bridge tests build `Event::Packet(Arc::new(…))`
by hand and call `translate` or an observer directly.

That style tests each reducer well. It cannot test the property most of
the port depends on: the engine applies a packet first and the port reads
the result. `last_block_updates`, the `known` entity map, the window
tracker's pending and early-content rules, and the death and respawn
re-arm in the bridge all exist because of that order. If an engine change
moves an apply into a later system or an earlier one, every unit test
still passes and the live suite (once a day) is the first to notice.

The engine already has the tool for this. `Simulation`
(`crates/mineflayer-client/src/test_utils/simulation.rs:52`) builds a
real app with the real plugins, accepts clientbound packets
(`receive_packet`), runs ticks, and records serverbound packets
(`SentPackets::expect`). The engine's own 26 simulation tests use it.

### Proposal: a port simulation harness

```rust
// src/test_util/sim.rs (new; behind cfg(test) and a `test-util` feature)
pub(crate) struct BotSim {
    pub sim: Simulation,       // the engine harness
    pub bot: Bot,              // a Bot over the simulation's client
    events: broadcast::Receiver<BotEvent>,
}

impl BotSim {
    pub fn new() -> Self;                               // login + spawn packets
    pub fn receive(&mut self, p: impl Packet<ClientboundGamePacket>);
    pub fn tick(&mut self);
    pub fn events(&mut self) -> Vec<BotEvent>;          // drain since last call
    pub fn sent(&self) -> &SentPackets;                 // serverbound checks
}
```

With it:

1. Record the event sequence of the current bridge for a fixed packet
   script (login, chunk, block updates, an entity spawn and removal, a
   window open/fill/close, a scoreboard, a death and respawn) as a golden
   file. F1 and F2 must reproduce the same sequence.
2. Test the typed windows (anvil, villager, enchanting table, furnace,
   book) by sending the packets a vanilla server would send. The live
   fixture cannot spawn these, so this is the only way to test them on
   every push.
3. Test the chat component defect of F3 with a `keybind` title packet.
4. Run all of it in the `verify` job. No Java, no network.

Export the harness behind a `test-util` cargo feature too, so that
third-party plugin authors (F4) can test their plugins the same way.

## The live suite

The live job runs on a schedule because it needs Java and a server jar.
Keep it, and add a cheap smoke subset (`lifecycle`, `chat`, `movement`)
to the push job once the jar cache is warm, so that a wire regression
fails the commit that caused it instead of the next morning's run.

## Docs

- `docs/api-parity.md` is thorough and current. It is also 654 lines of
  table, and the README is the only narrative introduction. Add a short
  "porting a mineflayer bot" guide that maps `bot.on`, `once`,
  `loadPlugin`, and the common option names to the Rust API, using the
  F5 helpers.
- The rustdoc is not hosted anywhere. Until D-publish changes, publish
  `cargo doc` output as a CI artifact or a static site.
- 33 comments cite plan IDs that point into replaced plans
  (`04_domain_modules.md`). State the rule in the comment; keep the ID in
  the commit message.

## Tooling and repository hygiene

- `target/` is 42 GB. The gate builds the engine with two feature sets.
  This is documented in the README; consider a separate target directory
  for the `log`-feature engine steps (`CARGO_TARGET_DIR`) so that one of
  them can be cleaned without a full rebuild.
- `.omo/` is untracked and not ignored (it shows in `git status`). Add
  it to `.gitignore`, or give it a nested `.gitignore` the way
  `.superpowers/sdd/` has one.
- `scripts/argon-unit.py` and `scripts/test-argon-unit.py` are personal
  tools; the README now says so. They are fine where they are.
