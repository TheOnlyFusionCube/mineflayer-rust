# Changelog

## Unreleased

### Breaking

- `Bot::new` is removed. It was `#[doc(hidden)]` and only the unit tests
  used it; they construct their scaffold bots through the crate's test
  utilities now.
- The engine is in this repository (`crates/mineflayer-*`). The crate no
  longer depends on an external engine and needs no `[patch]` table in a
  dependent workspace.
- The engine re-export is `mineflayer::engine` (was the upstream crate
  name). Engine paths in signatures are `mineflayer_engine::…`; the buffer
  derive and traits are `McBuf`/`McBufVar`/`McBufLimited`.
- `Auth::Microsoft` caches tokens in `~/.minecraft/mineflayer-auth.json`.
  The previous engine used a different file name, so the first connect
  after the update asks for the device-code login again (or rename the
  old cache file to keep the tokens).
- The packet-log env var is `MINEFLAYER_DO_NOT_CUT_OFF_PACKET_LOGS`.
- `mineflayer::engine` no longer has `swarm`, `ClientBuilder`, `JoinOpts`,
  `auto_reconnect`, `NoState`, `HandleFn`, or `BoxHandleFn`. The bot never
  used them; join with `Client::join` or `StartClientOpts`.
- `ContainerTarget::Block` carries the block's `kind: BlockKind` (was
  `name: String`); the container pre-check is a typed block set.
- Typed event payloads: `BotEvent::Kicked { reason, logged_in }` (was
  `Kicked(String, bool)`), `PathReset { reason: PathResetReason }` (was a
  `&'static str`; `as_str()` gives upstream's string),
  `PistonMove { direction: Direction }` (was the raw byte),
  `ResourcePack { uuid: Uuid }` and `BossBar::entity_uuid: Uuid` (were
  strings; boss bars are keyed by `Uuid`). `BotEvent::HardcodedSound`,
  which was never emitted, is removed.
- `Auth::Microsoft` has an `on_msa_code` field (`None` keeps the old
  behavior). `engine::auth::interactive_get_ms_auth_token` takes the
  callback as a fifth argument, and `AuthOpts` and
  `MicrosoftAccountOpts` have an `on_device_code` field.
- `engine::Event`, `engine::PathEvent`, `GotoEvent`, `PathfinderOpts`,
  `Pathfinder`, and `PathFoundEvent` are no longer `#[non_exhaustive]`.
  A match over `Event` must name the new `PacketDecodeFailed` variant.
- `engine::entity::inventory::Inventory` has two more public fields
  (`early_content`, `last_closed`), so a struct literal needs
  `..Default::default()`.

### Added

- `engine::ClientPlugins` and `StartClientOpts::new_with_plugins`: build
  the client app with plugins added or disabled.
- `engine::mining::CancelMiningEvent`: cancel queued and live mining,
  ordered against the start messages.
- `engine::Event::Pathfinder` carries pathfinder results.
- `engine::registry::tags::item_tag_by_name`.
- `engine::pathfinder::GotoId`: `start_goto` and `start_goto_with_opts`
  return it, and each `engine::PathEvent` carries the goto it belongs to
  (`PathFound { goto, .. }`, `GoalReached { goto }`, `Stopped { goto }`).
  `PathfinderOpts::total_timeout` and `PathEvent::Timeout { goto }` bound
  the total search time of one goto.
- `BotBuilder::plugin(plugin)`, the `bot.loadPlugin` analog: a Bevy plugin
  added to the bot's app after the engine's and the port's.
- `BotBuilder::respawn(bool)` (upstream `options.respawn`) and
  `BotBuilder::accept_resource_packs(bool)`: switch off the engine's
  auto-respawn and its accept-every-pack reply.
- `Bot::event_stream()`, a `Stream` of `BotEvent`s that yields
  `BotEvent::Lagged(n)` when the receiver lost `n` events, instead of
  hiding the loss. The examples use it.
- `Bot::wait_for` and `Bot::wait_map`, the analog of `once(bot, 'event')`
  with a time limit, and the public `WaitEnd` for why a wait ended.
- `Auth::Microsoft { on_msa_code }` (upstream `options.onMsaCode`): the
  Microsoft device code goes to a callback instead of stdout.
- `BotEvent::Error` when a game packet does not decode. The engine drops
  such a packet; before, only a `tracing` error log showed it. The engine
  side is `engine::Event::PacketDecodeFailed` and the ECS message
  `engine::packet::game::GamePacketDecodeFailedEvent`.
- `mineflayer::plugin`, what a plugin crate needs from a `Bot`:
  `Bot::state` and `Bot::state_mut` for per-bot state in a component,
  `Bot::ticks` (a `TickStream` that ends with the connection),
  `Bot::driver` (a handle that does not keep the connection open),
  `Bot::start_op` with `OpSlot` and `Op` (a newer start or a disconnect
  cancels the running action), and `Bot::plugin_events` with
  `Bot::emit_plugin_event` (a typed event channel per plugin event type).
  `tests/plugin.rs` is a plugin written against this API only.
- `Block::block_entity()` (upstream `block.blockEntity`) and
  `Bot::sign_text(&block, back)` (one side of `block.getSignText()`): the
  port keeps the block entities of the loaded chunks from the chunk and
  `block_entity_data` packets.
- `Bot::spawned()` (`await once(bot, 'spawn')`) and `Bot::chat_messages()`,
  a stream of upstream's `chat` `(username, message)` pairs.
- The `test-util` feature exports `test_util::sim::BotSim`, which runs a
  bot on the real engine and bridge with no server, for plugin tests.

### Changed

- `pathfinder().goto` waits for the outcome of its own goal and no longer
  polls each tick. Events of a replaced or stopped goal can no longer
  settle a newer `goto`. The `thinkTimeout` budget is the engine's
  `total_timeout`.
- Follow goals and `dynamic` goals step on each game tick inside the
  event bridge; the two background tasks per goal are gone.
- `engine::PathEvent::Timeout` has a `path` field (the partial path).
- `dig` ends on the engine's report for its own mining start, with no
  per-tick poll and no grace windows. A newer `dig` and `stop_digging`
  cancel it through the dig's operation.
- `engine::mining::StartMiningBlockEvent`, `MiningQueued`, `Mining`, and
  `FinishMiningBlockEvent` have an `id: MiningId` field. Each start ends in
  one `engine::mining::MiningFinished` message.
- `BotEvent::BlockUpdate.old` is the state the update replaced, also for
  the first update of a position (it reported `old == new` there) and for
  repeated updates of one position in one pass.
- The engine events of one ECS pass now reach the bot in a fixed order:
  raw packets first, then login, chunk loads, chat, the player list,
  spawn, death, and disconnect. Before, the order of these within one
  pass changed from run to run, so for example `BotEvent::Packet` for a
  login packet could come before or after `Connected` and `Login`.
- Keybind, selector, score, nbt, and object chat components no longer
  fail the whole packet. They decode to `FormattedText::Unresolved`, which
  keeps the fields and renders vanilla's fallback text. Before, a chat
  line, title, or container title with one of them was dropped, so for
  example `open_container` timed out on a server GUI with a keybind in
  its title. A match over `FormattedText` must name the new variant.
- The engine applies the inventory packets the bot used to patch in:
  `carried`/`state_id` from content and slot packets for the active menu,
  `set_player_inventory`, `set_cursor_item` (it was ignored), content
  before `open_screen`, and syncs under a closed window id. A window-0
  content packet while a container is open no longer replaces the cursor
  (vanilla behavior).
- Event waits that ignored `End` (`await_message`, the place waits, the
  creative slot wait, `wait_for_chunks_to_load`) now fail with
  `Error::NotConnected` when the bot ends, instead of waiting out their
  time limit.
- All domain observers see an engine event before any of its events go
  out, so a subscriber reads domain state that already reflects it.

### Fixed

- The `McBuf` derive generates field shorthand, so workspace clippy is
  clean. Three engine doctests that cannot compile are `ignore`.
- `connect` no longer tries to install a global log subscriber. The
  engine's `log` feature (bevy's log plugin) is off by default, so a
  dependent's own `tracing_subscriber` `init()` after `connect` cannot
  panic.
- `BotEvent::ResourcePack` also goes out for configuration-phase offers
  (the packs of server.properties and of proxies), from the new engine
  `Event::ResourcePack`. `accept_resource_pack` and `deny_resource_pack`
  answer in the phase the client is in, and with
  `accept_resource_packs(false)` they are the only answers, so a denial
  takes effect.
- `BotEvent::Login` goes out for each login packet, as upstream's
  `login` does; a proxy that moves the bot to another server gave none.
- `WindowUpdateSlot.old` is the stack the packet replaced in the engine's
  window, read before the engine applies the packet. After a click, the
  server's confirming slot update now reports the stack the click left,
  as upstream's `acceptClick` does, instead of the stack before the click.
- A login or a respawn now also clears the engine's buffered early window
  content and its last closed container, as upstream does; a later window
  that reuses an id no longer gets stale contents.
- The entity events read the metadata through the engine's typed decode,
  not by protocol index, so a metadata layout change in a new Minecraft
  version cannot misfire `entityCrouch`, `entityElytraFlew`, or
  `entitySleep`. `entityEffectEnd` reports the effect data the engine
  kept. The explosion-damage difficulty is `bot.game().difficulty`.

## 0.2.0 — 2026-10-16

The first holistic improvement plan (`docs/improvement_plan.md` at
commit `775ba66`). Breaking
changes first.

### Breaking

- `Error::Operation(String)` is removed. Match on `InvalidArgument`,
  `InvalidState`, `NotFound`, `Failed`, `Cancelled`, `Unsupported`, or
  `DigAborted(DigAbortReason)`. Each keeps the old message as `Display`.
- `Error::Connect` holds a `String` (no hickory type); thread-start
  failures are the new `Error::Runtime`.
- `Bot::spawn_point()` returns `Option<BlockPos>` (`None` before the
  server sends it).
- `Block::bounding_box()` returns `BoundingBox` (`as_str()` gives
  `"block"`/`"empty"`).
- `Bot::player_by_uuid` takes a `Uuid` (re-exported as `mineflayer::Uuid`).
- `update_held_item`, `close_window`, `activate_item`, and
  `deactivate_item` are no longer `async`.
- The 12 live test binaries are one binary, `live`.

### Added

- `mineflayer::prelude` and a re-export of the patched engine crate.
- `BotBuilder::packet_events(false)` keeps raw `BotEvent::Packet` off the
  event stream.
- `Block::short_name()`, `Bot::click_window_operation(ClickOperation)`.
- `tracing` logs (the crate emits; it never installs a subscriber).
- `examples/event_rate.rs` (event-volume and query-timing probe).

### Fixed

- Crate-internal waits no longer take the bootstrap event receiver, so the
  first `Bot::events()` call always gets the startup events.
- Background drivers (auto-eat, pvp, follow) no longer keep the connection
  open after the last user handle drops.
- `unequip(Hand)` after a disconnect returns `NotConnected` (was a panic).
- `transfer` rejects a negative count (was an unbounded click loop).
- `find_blocks`/`raycast` predicates run outside the world lock (deadlock).
- The bridge always sends one `End`, also when the engine's channel closes
  without a disconnect event.
- Locks are `parking_lot` (no poisoning); a written lock order.
- A cancelled `tab_complete` no longer leaks its correlation entry.
- Skin data is read with `serde_json` (the regexes missed some payloads).
- The entity tracker drops the old dimension's entries on respawn.

See `docs/improvement_plan.md` for the status of every item and
`docs/decisions.md` for the rulings.
