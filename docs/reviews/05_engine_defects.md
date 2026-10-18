# 05. Engine defects and missing switches

Finding F3 in [`audit_summary.md`](audit_summary.md), and the switch
items of F8. Baseline `ef06f74`. The engine is in this repository
(D-engine), so these are the port's defects too.

## Chat components fail whole packets

`FormattedText::from_nbt_compound`
(`crates/mineflayer-chat/src/component.rs:569-597`) returns `None` for
`score`, `selector`, `keybind`, `object`, and `nbt` components (there are
`TODO`/"aren't supported" traces at each). `McBuf for FormattedText`
(`:640-656`) turns `None` into `BufReadError::Custom("couldn't convert
nbt to chat message")`. Because one component anywhere in the tree fails
the parse, the whole packet fails to decode. `handle_raw_packet` then
returns the error, and the connection loop logs it with `error!` and goes
on (`crates/mineflayer-client/src/plugins/connection.rs:94-101`). The
packet is gone.

Every packet with a text field is affected: system chat, player chat,
titles, the action bar, boss bars, tab-list display names, scoreboard
display names, entity custom names in metadata, `open_screen` titles,
and disconnect reasons. Cases a user meets:

- A `keybind` component in any text. Vanilla's server sends keybinds
  unchanged (the client renders the key name), so this case breaks on a
  vanilla server too: a datapack or command that shows "press
  `[key.use]`" drops the packet.
- A `selector` or `score` component from server software, a proxy, or a
  plugin that does not resolve it before sending: no `Chat`, `Message`,
  or `MessageStr` event.
- A custom container (a server-side GUI) whose title has a keybind or
  score: no `open_screen`, so `open_container` waits 5 s and fails with
  `Timeout`, and the server thinks the window is open.
- An entity whose custom name has a selector: the whole
  `set_entity_data` packet is lost, so its other metadata is lost too.

Vanilla's server resolves `selector` and `score` before it sends
`tellraw` and `title` text, but not `keybind`, and other server software
sends all five as they are. Vanilla's client renders a `keybind` as the key name, a `selector`
as its pattern, a `score` as its stored value or nothing, and an `nbt`
component as nothing when it cannot resolve it. prismarine-chat accepts
all five. The engine should do the same: parse each into its own
`FormattedText` variant that keeps the raw data (so that `mc_write`
writes the same component back) and renders the vanilla fallback, and
never fail the packet. The JSON path (`Deserialize`, `:345-369`)
has the same gaps.

The port also does not know that a packet was dropped. Emit a
`BotEvent::Error` (or a new warning variant) with the packet id when a
game packet fails to decode, so that a user with no `tracing` subscriber
sees it.

## `#[non_exhaustive]` on enums the port owns

Command: `grep -rn non_exhaustive crates/mineflayer-engine/src`.

`Event` (`events.rs:61`), `PathEvent` (`events.rs:149`), two types in
`pathfinder/goto_event.rs` (`:20`, `:55`), and two in
`pathfinder/mod.rs` (`:116`, `:143`) are `#[non_exhaustive]`. The
attribute protects external users of a published crate from breaking
changes. Here the engine is unpublished and in the same workspace, so its
only effect is that the bridge needs wildcard arms
(`_ => { debug!(...); vec![] }` in `translate`, `_ => out` in
`tracking::bridge_events`). A new engine event is then dropped at run
time with a debug log instead of failing to compile. Remove the attribute
from these enums and remove the wildcard arms.

## Library code writes to stdout

`crates/mineflayer-auth/src/auth.rs:412` prints the Microsoft device code
with `println!`. An application with a TUI or a log format, or a bot
farm that logs in many accounts, cannot capture or route it. Upstream has
`options.onMsaCode` for this, which the port lists as `wontfix` ("no
interception seam"). The seam is one parameter: let the device-code
function take a callback (or return the code through a channel), and let
`Auth::Microsoft` carry it. Keep `println!` as the default callback.

## Wrong or risky details in the pathfinder

- The doc of `PathEvent::PathFound` (`events.rs:151-153`) says that an
  empty `path` with `is_partial == false` means no route. The A* returns
  an empty *partial* path for no route (`pathfinder/mod.rs:561-590`), and
  the port's `classify_search_result` assumes the latter. Correct the
  doc.
- `calculate_path` sleeps 100 ms on the search thread after every
  partial result "so it's not a busy loop" (`pathfinder/mod.rs:567`).
  `Pathfinder::get_path_from_to` runs `calculate_path` on the caller's
  thread, so a preview that ends partial sleeps the caller for 100 ms; on
  the bot thread that stalls the ECS. Move the sleep to the repath loop
  that needs it.
- `calculate_path` has an `expect("path should always still be
  possible…")` (`:601`). If the world cache and the successors function
  ever disagree, the search thread panics. Return `None` and log instead.

## Unbounded channels

The engine's `Event` sender (`client_impl/mod.rs:210`) and the port's
receiver (`src/bot/connection.rs:334`) are unbounded. At the measured
peak (8,751 events/s, `docs/performance.md`) a stalled bridge grows the
queue without limit. F1 removes this channel from the port's input. If F1
is not done, bound it and count drops.

## Switches that need no engine change

`StartClientOpts::new_with_plugins` (`client_impl/mod.rs:114`) builds the
app from any `PluginGroup`, and Bevy's `PluginGroupBuilder::disable::<P>()`
removes a plugin from a group. So the port can offer these upstream
options with builder flags and no engine edit:

| Upstream option | Plugin to disable | Parity row today |
|---|---|---|
| `options.respawn: false` | `AutoRespawnPlugin` (`auto_respawn.rs:12`) | `diverges` ("no toggle exposed") |
| `bot.denyResourcePack()` that works | `AcceptResourcePacksPlugin` (`accept_resource_packs.rs:24`), then answer pushes in a port system | `diverges` ("a later denial usually has no effect") |
| `options.physicsEnabled: false` | the physics plugin, if the movement plugins tolerate its absence (test first) | `wontfix` |

The config-phase resource pack gap (`docs/api-parity.md:526-528`) closes
the same way: a port system reads `ReceiveConfigPacketEvent`, which the
engine already sends (`plugins/packet/mod.rs:43`).
