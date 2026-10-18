# Porting a mineflayer bot to Rust

This guide maps the parts of the mineflayer API that every bot uses to
their Rust forms. Each example compiles as a doctest of the crate
(`PortingDoctests` in `src/lib.rs`). [`api-parity.md`](api-parity.md) has
the row for each upstream API.

The general rules:

- A method that returns a promise is an `async fn` that returns
  `mineflayer::Result<T>`. `await` it.
- A `camelCase` name is `snake_case`: `bot.blockAt(pos)` is
  `bot.block_at(pos)`, `bot.entity.position` is `bot.position()`.
- An event is a `BotEvent` variant with the event's arguments as fields:
  `'blockUpdate'` is `BotEvent::BlockUpdate { old, new }`.
- A plugin is a crate. Its methods come from an extension trait that you
  import.

## `mineflayer.createBot(options)`

`Bot::builder()` takes the options as methods, and `connect()` resolves
when the bot has logged in.

```js
const bot = mineflayer.createBot({
  host: 'localhost', port: 25565, username: 'bob',
  auth: 'offline', respawn: false, brand: 'my-bot',
})
```

```rust,no_run
use mineflayer::{Auth, Bot, Result};

async fn create() -> Result<Bot> {
    Bot::builder()
        .host("localhost")
        .port(25565)
        .username("bob")
        .auth(Auth::Offline {
            username: "bob".into(),
        })
        .respawn(false)
        .brand("my-bot")
        .connect()
        .await
}
```

| Option | Builder method |
|---|---|
| `host`, `port`, `username` | `host`, `port`, `username` |
| `auth: 'offline'` / `'microsoft'` | `auth(Auth::Offline { .. })` / `auth(Auth::Microsoft { .. })` |
| `onMsaCode` | the `on_msa_code` field of `Auth::Microsoft` |
| `version` | none: the engine supports one Minecraft version |
| `respawn` | `respawn(bool)` |
| `physicsEnabled` | `physics(bool)` |
| `plugins` | `plugin(impl Plugin)`, see [Plugins](#botloadpluginplugin) |
| `viewDistance`, `chat`, `colorsEnabled`, `skinParts`, `mainHand` | the methods of the same names |
| `brand`, `difficulty`, `chatLengthLimit`, `defaultChatPatterns`, `checkTimeoutInterval` | the methods of the same names |

## `bot.on(event, listener)`

`bot.event_stream()` is every event in order. A `match` takes the place of
the listeners. A receiver that falls behind gets `BotEvent::Lagged(n)` in
place of the `n` events it lost.

```js
bot.on('spawn', () => bot.chat('hi'))
bot.on('health', () => console.log(bot.health))
bot.on('end', reason => console.log('ended', reason))
```

```rust,no_run
use futures::StreamExt;
use mineflayer::{Bot, BotEvent};

async fn listen(bot: Bot) {
    let mut events = bot.event_stream();
    while let Some(event) = events.next().await {
        match event {
            BotEvent::Spawn => {
                let _ = bot.chat("hi");
            }
            BotEvent::Health => println!("{}", bot.health()),
            BotEvent::End(reason) => {
                println!("ended {reason}");
                break;
            }
            _ => {}
        }
    }
}
```

`bot.on('chat', (username, message) => …)` has its own stream of player
lines:

```rust,no_run
use futures::StreamExt;
use mineflayer::Bot;

async fn echo(bot: Bot) {
    let mut chat = bot.chat_messages();
    while let Some((username, message)) = chat.next().await {
        if username != bot.username() {
            let _ = bot.chat(&message);
        }
    }
}
```

## `await once(bot, event)`

`bot.spawned()` is `once(bot, 'spawn')`. `bot.wait_for(timeout, predicate)`
waits for any event, with a time limit.

```js
await once(bot, 'spawn')
const [oldBlock, newBlock] = await once(bot, 'blockUpdate')
```

```rust,no_run
use std::time::Duration;

use mineflayer::{Bot, BotEvent};

async fn wait(bot: Bot) {
    bot.spawned().await.expect("the bot spawned");
    let update = bot
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, BotEvent::BlockUpdate { .. })
        })
        .await;
    if let Ok(BotEvent::BlockUpdate { old, new }) = update {
        println!("{:?} became {:?}", old.map(|b| b.kind()), new.kind());
    }
}
```

## `bot.loadPlugin(plugin)`

The upstream plugins `mineflayer-tool`, `mineflayer-auto-eat`,
`mineflayer-collectblock`, and `mineflayer-pvp` are in the
`mineflayer-plugins` crate. Add it as a dependency and import its
prelude; there is no load step. `mineflayer-pathfinder` is in the core
crate (`bot.pathfinder()`).

```js
bot.loadPlugin(require('mineflayer-pathfinder').pathfinder)
bot.loadPlugin(require('mineflayer-pvp').plugin)
bot.pvp.attack(entity)
bot.pathfinder.goto(new goals.GoalNear(10, 64, 10, 1))
```

```rust,no_run
use mineflayer::{Bot, EntityRef, Goal, Result, Vec3};
use mineflayer_plugins::prelude::*;

async fn fight(bot: Bot, entity: EntityRef) -> Result<()> {
    bot.pvp().attack(&entity)?;
    bot.pathfinder()
        .goto(Goal::Near {
            pos: Vec3::new(10., 64., 10.),
            radius: 1.,
        })
        .await
}
```

To write a plugin, see "Writing a plugin" in the README: a Bevy plugin
added with `BotBuilder::plugin`, and an extension trait on `Bot` built on
`mineflayer::plugin`.

## Errors

A rejected promise is an `Err(mineflayer::Error)`. Upstream's `err.name`
values are variants, so a `match` takes the place of a string compare.

```js
try { await bot.pathfinder.goto(goal) } catch (err) {
  if (err.name === 'NoPath') console.log('no path')
}
```

```rust,no_run
use mineflayer::{Bot, Error, Goal, PathError};

async fn walk(bot: Bot, goal: Goal) {
    match bot.pathfinder().goto(goal).await {
        Ok(()) => {}
        Err(Error::Path(PathError::NoPath)) => println!("no path"),
        Err(Error::NotConnected) => println!("the connection ended"),
        Err(other) => println!("{other}"),
    }
}
```

| Upstream | `Error` |
|---|---|
| a promise that never resolves | `Timeout` (the port bounds its waits) |
| `err.name` of the pathfinder | `Path(PathError::{NoPath, Timeout, GoalChanged, PathStopped})` |
| `'Digging aborted'` | `DigAborted(reason)` |
| an assertion on an argument | `InvalidArgument(message)` |
| a call in the wrong state | `InvalidState(message)` |
| the connection ended | `NotConnected` |
