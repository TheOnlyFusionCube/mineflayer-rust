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
- `Bot::wait_for` and `Bot::wait_map`, the analog of `once(bot, 'event')`
  with a time limit, and the public `WaitEnd` for why a wait ended.
- `Auth::Microsoft { on_msa_code }` (upstream `options.onMsaCode`): the
  Microsoft device code goes to a callback instead of stdout.
- `BotEvent::Error` when a game packet does not decode. The engine drops
  such a packet; before, only a `tracing` error log showed it. The engine
  side is `engine::Event::PacketDecodeFailed` and the ECS message
  `engine::packet::game::GamePacketDecodeFailedEvent`.

### Changed

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
