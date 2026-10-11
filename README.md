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
`EntitySpawn`, `WindowUpdateSlot`, ~80 variants). Domain methods live in
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
semantic difference, with a note), `wontfix`, or `pending`. Current counts:
**321 ported · 78 diverges · 34 wontfix · 40 pending**.

## Live testing

`scripts/live-server.sh` boots a local vanilla **offline-mode** server
(MC `26.1`, port `25566`, state in `.live/`) and prints `LIVE_SERVER_READY`.
Live tests in `tests/live/` are gated behind the `live` feature and
`#[ignore]`:

```sh
bash scripts/live-server.sh
cargo test --features live -- --ignored
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
