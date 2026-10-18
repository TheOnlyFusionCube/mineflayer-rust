# mineflayer-rs

A Rust port of the [mineflayer](https://github.com/PrismarineJS/mineflayer)
Minecraft bot API, built on an in-tree client engine (`crates/`, Bevy-ECS:
protocol, world state, physics, pathfinding).
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
| `plugin` | the API a plugin crate builds on (see [Writing a plugin](#writing-a-plugin)) |

The mineflayer-tool, -auto-eat, -collectblock, and -pvp analogs are in
the `mineflayer-plugins` crate (`crates/mineflayer-plugins`), one cargo
feature each. Like their upstream packages, they are a second
dependency:

```rust,no_run
use mineflayer::{Bot, Result};
use mineflayer_plugins::{prelude::*, pvp::PvpEvent};

async fn guard(bot: Bot, target: mineflayer::EntityRef) -> Result<()> {
    let mut pvp_events = bot.plugin_events::<PvpEvent>();
    bot.pvp().attack(&target)?;
    while let Ok(event) = pvp_events.recv().await {
        if event == PvpEvent::StoppedAttacking {
            break;
        }
    }
    Ok(())
}
```

## Quick start

```rust,no_run
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

Runnable ports of 23 upstream examples live in `examples/`: `echo`,
`digger`, `chest`, `inventory`, `sleeper`, `fisherman`, `farmer`, `guard`,
`trader`, `book`, `anvil`, `scoreboard`, `titles`, `tab_complete`,
`blockfinder`, `raycast`, `sound`, `multiple`, `reconnector`, `pathfinder`
(upstream's `pathfinder/gps.js`), `collectblock`, `auto_eat`, and
`attack`; `event_rate` measures the event throughput. Run one against a
local offline-mode server:

```sh
cargo run --example echo -- localhost 25566
```

## Porting a bot

[`docs/porting.md`](docs/porting.md) maps `createBot` options,
`bot.on`, `once`, `loadPlugin`, and upstream's error names to the Rust
API, with an example for each that compiles as a doctest. CI uploads
the API docs (`cargo doc`) as the `api-docs` artifact of each run.

## Writing a plugin

mineflayer's `bot.loadPlugin(plugin)` takes a function that adds methods
and listeners to the bot at run time. In Rust a plugin is a crate with
two optional halves:

- an ECS half, a Bevy plugin added at build time with
  `BotBuilder::plugin`, whose systems run in the bot's engine schedule;
- an async half, an extension trait on `Bot` built on `mineflayer::plugin`:
  per-bot state in a component (`Bot::state`, `Bot::state_mut`), the game
  ticks (`Bot::ticks`), operations that a newer call or a disconnect
  cancels (`OpSlot`, `Bot::start_op`), and the plugin's own event type
  (`Bot::emit_plugin_event`, `Bot::plugin_events`).

```rust
use mineflayer::{
    Bot,
    engine::ecs::{self as bevy_ecs, component::Component},
    plugin::OpSlot,
};

/// Per-bot state, created on first use and dropped with the connection.
#[derive(Component, Default)]
struct Greeter {
    greetings: u32,
    op: OpSlot,
}

/// The plugin's event type; not a `BotEvent` variant.
#[derive(Clone, Debug)]
struct Greeted(u32);

trait GreeterExt {
    async fn greet_every_tick(&self, times: u32);
}

impl GreeterExt for Bot {
    async fn greet_every_tick(&self, times: u32) {
        // a second call cancels the first, and so does the disconnect
        let Some(op) = self.state_mut(|g: &mut Greeter| self.start_op(&mut g.op)) else {
            return;
        };
        let mut ticks = self.ticks_until(&op);
        for _ in 0..times {
            if !ticks.next().await {
                return;
            }
            let n = self.state_mut(|g: &mut Greeter| {
                g.greetings += 1;
                g.greetings
            });
            self.emit_plugin_event(Greeted(n.unwrap_or_default()));
        }
        self.state_mut(|g: &mut Greeter| g.op.finish(&op));
    }
}
```

`tests/plugin.rs` runs a plugin like this one on `BotSim` (the
`test-util` feature), with the real engine and no server. The crate
`mineflayer-plugins` is the larger example: four upstream plugins on
this API only.

## Parity status

[`docs/api-parity.md`](docs/api-parity.md) is the source of truth: every
public mineflayer API is a row marked `ported`, `diverges` (intentional
semantic difference, with a note), `wontfix`, or `additive` (Rust surface
beyond upstream). Current counts:
**348 ported · 124 diverges · 41 wontfix · 11 additive — zero pending**.

## Live testing

`scripts/live-server.sh` boots a local vanilla **offline-mode** server
(MC `26.1`, port `25566`, state in `.live/`) and prints `LIVE_SERVER_READY`.
`reset` stops it, deletes the `.live/world*` dimensions, and starts a fresh
world with seed `13371337`; use it before a deterministic live run.
After boot the script sets the `respawn_radius` gamerule to 0 over RCON
(bound to `127.0.0.1`, random password per start), so every bot joins at
the same world spawn instead of a random point in a 21×21 area. All 14
live tests run in one binary, `live`.
Live tests in `tests/live/` are gated behind the `live` feature and
`#[ignore]`:

```sh
bash scripts/live-server.sh reset
cargo test --features live --no-fail-fast -- --ignored --test-threads=1
./scripts/live-server.sh stop
```

CI (`.github/workflows/verify.yml`) runs the same three commands in the
`live` job, daily and on manual dispatch, with Java 25 installed and the
server jar cached.

Examples can be pointed at the same fixture
(`cargo run --example digger -- localhost 25566`). Never point them at real
Mojang endpoints or public servers without `-- online` auth and your own
account.

## Toolchain

Nightly only, pinned in `rust-toolchain.toml` to **nightly-2026-10-01**.
The engine crates need nightly (edition 2024 + feature gates); the latest
nightly at pin time ICEs compiling the entity crate, and
`.cargo/config.toml` sets `-Znext-solver=no` because the next-gen trait
solver can't infer `mineflayer-core`'s const generics. Both pins are
workspace-local — don't override with `cargo +nightly` or `RUSTFLAGS`.

The gate builds the engine crates with two feature sets: the `log` steps
build into `target/engine-log`, everything else into `target/`. Remove
either directory on its own when it grows large; the other stays built.

## Layout

The repository is one Cargo workspace. The root package is `mineflayer`
(the bot API). `crates/mineflayer-*` is the client engine under it:
`mineflayer-engine` is the facade the port uses, re-exported as
`mineflayer::engine`; the other crates hold the protocol, registries,
world, physics, inventory, and auth layers. `docs/version_upgrade.md`
covers how the engine moves to a new Minecraft version.

`ref/` (git-ignored) holds read-only source checkouts for review.
`ref/mineflayer` is the upstream JavaScript source the port follows.

## Use as a dependency

The engine is part of this repository, so a dependent crate needs no
`[patch]` table. It must still use the same toolchain settings, because
Cargo reads them per workspace:

1. `rust-toolchain.toml` — the pinned nightly:

   ```toml
   [toolchain]
   channel = "nightly-2026-10-01"
   components = ["rustfmt", "clippy"]
   ```

2. `.cargo/config.toml` — the trait-solver flag:

   ```toml
   [build]
   rustflags = ["-Znext-solver=no"]
   ```

Name engine types through `mineflayer::engine` (or the `mineflayer::prelude`
re-exports) so that they always match the version the bot uses.

## Limitations

- **One Minecraft version.** The engine targets one MC release
  (`0.16.0+mc26.1` → MC `26.1`); mineflayer's multi-version `supportFeature`/version-choice
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

## Maintainer tools

`scripts/argon-unit.py` and `scripts/test-argon-unit.py` send a brief
plus source files to a local model API and write the returned files into
`src/`, `docs/`, or `tests/`. They are personal workflow tools with no
role in building or testing the crate — nothing in the build, the examples,
or the gate reads them. Run them only on a clean tree and review every
written file before committing.

## License

MIT. The API surface is derived from mineflayer (MIT,
© PrismarineJS contributors). Third-party notices for code in `crates/`
are in `LICENSE`.
