# mineflayer-rs

A Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
Minecraft bot API, built on top of [azalea](https://github.com/azalea-rs/azalea)
(Bevy-ECS client framework, protocol, physics, pathfinding). Consumers are Rust
authors porting mineflayer bot code.

**Status: scaffold.** `Bot` (a cloneable handle over `azalea::Client`) and
`BotEvent` (broadcast event stream) exist; the domain surface is tracked in
[`docs/api-parity.md`](docs/api-parity.md) — every mineflayer API row is marked
`pending`/`ported`/`diverges`/`wontfix`. See [`docs/PORT-PLAN.md`](docs/PORT-PLAN.md)
for the architecture and unit plan.

```rust,no_run
// Sketch (API not yet implemented):
let bot = mineflayer::Bot::builder()
    .username("echo_bot")
    .connect("localhost:25566")
    .await?;
let mut events = bot.events();
while let Ok(event) = events.recv().await {
    if let mineflayer::BotEvent::Chat(chat) = event {
        // bot.chat(...) — echo back
    }
}
```

## Toolchain

Pinned nightly (`rust-toolchain.toml`): **nightly-2026-10-01**. azalea requires
nightly, and the latest nightly at scaffold time (2026-10-09) ICEs compiling
`azalea-entity`; `.cargo/config.toml` additionally forces `-Znext-solver=no`
(next-gen trait solver can't infer azalea-core's const generics). Both are
workspace-local — don't override with `cargo +nightly` / `RUSTFLAGS`.

## Scripts

- `scripts/verify.sh` — per-unit gate: `fmt --check`, `clippy -D warnings`,
  `test`, `build`.
- `scripts/live-server.sh` — boots a local vanilla **offline-mode** server
  (MC `26.1`, port `25566`, state in `.live/`); prints `LIVE_SERVER_READY`
  when up. `./scripts/live-server.sh stop` kills it. Live tests use this
  fixture only — never real Mojang endpoints.
