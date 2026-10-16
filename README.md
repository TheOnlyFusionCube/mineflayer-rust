# mineflayer-rs

A Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
Minecraft bot API, built on [azalea](https://github.com/azalea-rs/azalea)
(Bevy-ECS client framework: protocol, world state, physics, pathfinding).
Written for Rust authors porting mineflayer bots: the public surface keeps
mineflayer's names and semantics wherever Rust idiom allows.

## API shape

`Bot::builder()` is `mineflayer.createBot(options)` — host, port, username,
`Auth::{Offline, Microsoft, Account}`, view distance. `connect()` returns a
cheap-cloneable `Bot` running on its own thread. `bot.events()` is
`bot.on(...)`: a broadcast stream of `BotEvent` (`Spawn`, `Chat`, `End`,
`EntitySpawn`, `WindowUpdateSlot`, 110+ variants). Domain methods live in
`impl Bot` blocks per module, mirroring mineflayer's docs structure:

| Module | Covers |
|---|---|
| `chat` | `chat`, `whisper`, `await_message`, chat patterns, `tab_complete`, `tablist` |
| `world` | `block_at`, `find_block`/`find_blocks`, raycasts, signs, `time`, weather, `game` |
| `entity` | `entities`, `players`, `attack`, `activate_entity`, `get_explosion_damages` |
| `movement` | `set_control_state`, `look`/`look_at`, physics/abilities reads |
| `interact` | `dig`/`stop_digging`/`dig_time`, `place_block`/`place_entity`, `activate_block` |
| `pathfinder` | `goto`-style goals, `Movements`, path events |
| `inventory` | `inventory`, `held_item`, equip/toss/click/transfer, `open_container` |
| `windows` | `Window`/`Item`/`Chest`/`Dispenser` — slots, deposit/withdraw, `close` |
| `window_types` | `Furnace`, `EnchantmentTable`, `Anvil`, `Villager`, books, command blocks |
| `craft` | `recipes_for`/`recipes_all`/`craft` (server recipe book) |
| `misc` | settings, bed/sleep, `fish`, creative mode, resource packs |
| `scoreboard` | `ScoreBoard`/`ScoreItem`/`Team`/`BossBar` |

## Quick start

```rust
// examples/echo.rs — echoes chat back to the server.
use mineflayer::{Bot, BotEvent};

#[tokio::main]
async fn main() {
    let bot = Bot::builder()
        .host("localhost")
        .port(25566)
        .username("echo")
        .connect()
        .await
        .expect("connect failed");

    let mut events = bot.events();
    loop {
        match events.recv().await {
            Ok(BotEvent::Chat(packet)) => {
                let (Some(sender), content) = packet.split_sender_and_content() else {
                    continue;
                };
                if sender != bot.username() {
                    let _ = bot.chat(&content);
                }
            }
            Ok(BotEvent::End(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
}
```

Runnable ports of upstream examples live in `examples/`: `echo.rs`,
`digger.rs` (dig/build/equip on chat command), `chest.rs` (open a chest,
deposit/withdraw). Run one against a local offline-mode server:

```sh
cargo run --example echo -- localhost 25566
```

## Parity status

[`docs/api-parity.md`](docs/api-parity.md) is the source of truth: every
public mineflayer API is a row marked `ported`, `diverges` (intentional
semantic difference, with a note), `wontfix`, or `additive` (Rust surface
beyond upstream). Current counts:
**344 ported · 122 diverges · 49 wontfix · 11 additive — zero pending**.

## Live testing

`scripts/live-server.sh` boots a local vanilla **offline-mode** server
(MC `26.1`, port `25566`, state in `.live/`) and prints `LIVE_SERVER_READY`.
`reset` stops it, deletes the `.live/world*` dimensions, and starts a fresh
world with seed `13371337`; use it before a deterministic live run.
Live tests in `tests/live/` are gated behind the `live` feature and
`#[ignore]`:

```sh
bash scripts/live-server.sh reset
cargo test --features live --no-fail-fast -- --ignored --test-threads=1
./scripts/live-server.sh stop
```

Examples can be pointed at the same fixture
(`cargo run --example digger -- localhost 25566`). Never point them at real
Mojang endpoints or public servers without `-- online` auth and your own
account.

## Toolchain

Nightly only, pinned in `rust-toolchain.toml` to **nightly-2026-10-01**.
azalea requires nightly (edition 2024 + feature gates); the latest nightly
at pin time ICEs compiling `azalea-entity`, and `.cargo/config.toml` sets
`-Znext-solver=no` because the next-gen trait solver can't infer
azalea-core's const generics. Both pins are workspace-local — don't
override with `cargo +nightly` or `RUSTFLAGS`.

`Cargo.toml`'s `[patch.crates-io]` also pins every `azalea*` crate to
upstream commit `ddef371` (still the `0.16.0+mc26.1` release line): the
published release serializes `ServerboundAttack`'s entity id as a fixed
i32 where release MC 26.1 servers read a VarInt, so every `bot.attack()`
was rejected as malformed and kicked the client. Drop the patch once a
fixed release is published.

`ref/` (git-ignored) holds read-only source checkouts for review:
`ref/azalea` must be at the pinned commit `ddef371`
(`git -C ref/azalea checkout ddef371`), because the azalea `main` branch
is already on a newer Minecraft version and reads differently.
`ref/mineflayer` is the upstream JavaScript source the port follows.

## Use as a dependency

Cargo applies `[patch.crates-io]` and toolchain settings per workspace, so
a crate that depends on `mineflayer` must repeat three things in its own
workspace. Without the patch, the published azalea `0.16` serializes
`ServerboundAttack` incorrectly and every `bot.attack()` is kicked as
malformed (see Toolchain above).

1. `Cargo.toml` — the patch table:

   ```toml
   [patch.crates-io]
   azalea = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-auth = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-block = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-block-macros = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-brigadier = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-buf = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-buf-macros = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-chat = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-client = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-core = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-crypto = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-entity = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-inventory = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-inventory-macros = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-language = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-physics = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-protocol = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-protocol-macros = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-registry = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-registry-macros = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   azalea-world = { git = "https://github.com/azalea-rs/azalea", rev = "ddef37118448b639ff56b86ae339e8913cb4ed11" }
   ```

2. `rust-toolchain.toml` — the pinned nightly:

   ```toml
   [toolchain]
   channel = "nightly-2026-10-01"
   components = ["rustfmt", "clippy"]
   ```

3. `.cargo/config.toml` — the trait-solver flag:

   ```toml
   [build]
   rustflags = ["-Znext-solver=no"]
   ```

The gate (`scripts/verify.sh`) plus the live suite is the port's own
verification; a dependent crate verifies its patch by attacking a spawned
entity against the fixture (`scripts/live-server.sh`).

## Limitations

- **One Minecraft version.** azalea tracks latest MC only (`0.16.0+mc26.1`
  → MC `26.1`); mineflayer's multi-version `supportFeature`/version-choice
  APIs are `diverges` by construction.
- **`diverges` rows are real.** Prominent ones: `event('chat')` is a raw
  always-on packet event (the derived `Message`/`MessageStr` family is the
  strict upstream semantic); `recipes_for`/`recipes_all` see only
  server-unlocked recipe-book entries; `bot.physics` is read-only;
  `find_player` returns `Option`, not upstream's union type. Read
  api-parity.md before porting code that touches them.
- **No `bot.on(...)` emitter on `Window`.** Window events are `BotEvent`
  variants carrying the protocol window id.
- Windows/entities/actions that need server-side objects a vanilla fixture
  can't produce (real chests, riding, explosions) are unit-tested and
  source-reviewed but not all live-exercised — see the unit reports.

## License

MIT. The API surface is derived from mineflayer (MIT,
© PrismarineJS contributors); the implementation builds on azalea (MIT).
