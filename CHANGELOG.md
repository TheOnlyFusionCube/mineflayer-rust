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
