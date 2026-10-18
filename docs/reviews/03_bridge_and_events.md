# 03. The bridge, the event API, and the extension model

Findings F4 and F5 in [`audit_summary.md`](audit_summary.md). Baseline
`ef06f74`.

## The bridge after the second plan

`src/bridge.rs` has 517 production lines. `OBSERVERS` (line 220) is one
ordered list of eleven adapters, which is a clear improvement on the
seven-shape loop the second audit found. Five things still belong
somewhere else.

1. **Chat logic in the bridge.** `translate` builds the chat message
   family for `Event::Chat` (`state.chat.lock().message_events`), and
   `packet_side_events` decodes `SetActionBarText`, `TabList`, and
   `CommandSuggestions` into chat state. Every other domain has its
   observer in its own module; chat has none.
2. **A duplicate handle.** `BridgeState.chat` is a clone of
   `shared.chat`, while the `Observe` context already carries `shared`.
3. **Connection policy in the event loop.** The `minecraft:brand`
   payload encode and the keepalive watchdog (`options.checkTimeoutInterval`)
   sit inside the `select!` loop of `spawn`, which is 121 lines.
4. **Player-list membership in the bridge.** `BridgeState.players` mirrors
   upstream's `bot.players` keys only to gate `playerJoined` and
   `playerLeft`. It is tab-list state and belongs with `tablist`.
5. **One packet, two decoders.** `ClientboundBlockEvent` is decoded by
   `world::block_event` (pistons, chest lids) and by
   `misc::handle_packet_state` (note blocks), with a comment in each that
   names the other.

F1's plugin group dissolves most of this: each item becomes a system in
its domain plugin. If F1 is not done, move items 1, 2, 4, and 5 into the
chat and world modules, and move item 3 into a `connection` observer.

## The `BotEvent` enum

123 variants in one `#[non_exhaustive]` enum (`src/bot/event.rs`, 650
lines). D-events keeps one enum on purpose, and that is reasonable for
the core events. The problems are at the edges.

- **Plugin events in the core enum.** `CollectBlockFound`, `Collected`,
  `CollectDone`, `CollectBlockFinished`, `StartedAttacking`,
  `StoppedAttacking`, `AttackedTarget`, `EatStart`, `EatFail`,
  `EatFinish`, and the five path events belong to plugins that upstream
  ships as separate packages. A third-party plugin has no way to add a
  variant (the enum is `#[non_exhaustive]` and lives in this crate).
- **A dead variant.** `HardcodedSound` is documented as never emitted.
- **Weak payloads.** `PathReset { reason: &'static str }` has two possible
  values and should be an enum. `Kicked(String, bool)` hides the meaning
  of the bool (upstream's `loggedIn`); a struct variant names it.
  `PistonMove { direction: u8 }` is a raw protocol byte; the engine has a
  `Direction` type. Boss bars are keyed by `String` although the packet
  carries a `Uuid`. `ResourcePack.uuid` is a `String` for the same
  reason.
- **Inconsistent paths.** The file imports `EntityRef` but writes
  `mineflayer_engine::EntityRef` 35 times.

## Developer experience

Every example and every live test writes the same loop:

```rust
loop {
    match events.recv().await {
        Ok(BotEvent::Chat(packet)) => { /* ... */ }
        Ok(BotEvent::End(_)) => break,
        Ok(_) => {}
        Err(broadcast::error::RecvError::Lagged(_)) => {}
        Err(broadcast::error::RecvError::Closed) => break,
    }
}
```

Upstream users write `bot.on('chat', (username, message) => …)` and
`await once(bot, 'spawn')`. The crate has the wait helper that would close
most of the gap, `bot::wait::await_event` and `await_map`, but they are
`pub(crate)`. Three changes make the public API as short as upstream's
without losing types.

1. `Bot::wait_for(timeout, |e| matches!(e, BotEvent::Spawn))` and
   `Bot::wait_map(timeout, |e| …)`, public, returning
   `Result<T, WaitEnd>` (the type already exists).
2. `Bot::event_stream()` returning `impl Stream<Item = BotEvent>` that
   counts lagged events instead of hiding them, so that
   `while let Some(e) = stream.next().await` is the whole loop. Report
   the lost count through a `BotEvent::Lagged(u64)` item or a counter on
   the stream.
3. Typed helpers for the two events almost every bot uses: a chat stream
   that yields `(sender, message)` the way upstream's `chat` event does,
   and `Bot::spawned()` that resolves on the first `Spawn`.

## The extension model

The parity table marks `bot.loadPlugin`, `bot.loadPlugins`,
`bot.hasPlugin`, `options.plugins`, and `options.loadInternalPlugins` as
`wontfix` with the note "compile-time plugin model"
(`docs/api-parity.md:58-95`). No such model is documented, and the
in-tree plugin analogs show why a third party could not write one:

| A plugin needs | The in-tree analogs use | Public? |
|---|---|---|
| per-bot state | `with_component` / `with_component_mut` on the client entity | no (`pub(crate)`) |
| a tick loop that stops on disconnect | `TickStream`, `Bot::shutdown` | no |
| a handle that does not keep the bot alive | `Bot::driver_handle` | no |
| to emit events | `Bot::emit_event` into `BotEvent` | no, and the enum is closed |
| to see packets in order | an entry in `OBSERVERS` | no |
| an ECS system | `StartClientOpts::new_with_plugins` | engine has it; `BotBuilder` does not expose it |

### Proposal

1. `BotBuilder::plugin(impl Plugin)` passes Bevy plugins to
   `new_with_plugins`. This is the `loadPlugin` analog for anything that
   wants ECS access; it costs a few lines because the engine hook exists.
2. A small public `mineflayer::plugin` module: `Bot::state::<T>()` and
   `Bot::state_mut::<T>()` over the existing component helpers, the
   operation primitive from F2 (`Bot::start_op`), and
   `Bot::plugin_events::<E>()` that returns a typed broadcast for a
   plugin's own event type `E`. Plugin events then leave the core enum.
3. Move `pathfinder`, `pvp`, `collect`, `auto_eat`, and `tool` into a
   separate workspace crate (`mineflayer-plugins`, or one crate each,
   mirroring upstream's packages) that uses only the public surface. If
   the move needs a private item, the public surface is missing
   something, and the move finds it. The core crate re-exports the
   plugins behind default features so `mineflayer::pathfinder` keeps
   working.
4. Flip the five `wontfix` rows to `diverges` ("a Bevy plugin added at
   build time; no runtime loader") and document the model in the README.

`pathfinder` is the hardest to move, because the facade's goal state and
the follow systems of F2 are engine-adjacent. Do it after F2.
