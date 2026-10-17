# 02. The bridge and the event stream

Baseline `7a2bc95`. Evidence for summary finding F2.

## Observer shapes today

```sh
grep -rnE 'pub\(crate\) fn (observe_event|bridge_events|after_event)' src
```

| Domain | Entry point | Input | Output | State |
|---|---|---|---|---|
| movement | `movement::bridge_events(&client, &mut MovementTracker, &Event)` | engine event | `Vec<BotEvent>` | bridge-local |
| pathfinder | `pathfinder::bridge_events(&client, &mut PathTracker, &Event)` | engine event | `Vec<BotEvent>` | bridge-local + ECS facade |
| entity | `entity::observe_event(&Arc<RwLock<EntityTracker>>, &client, &Event)` | engine event | `Vec<BotEvent>` | `BotShared` |
| windows | `windows::observe_event(&Arc<Mutex<WindowTracker>>, &client, &Event)` | engine event | `Vec<BotEvent>` | `BotShared` + ECS writes |
| window types | `window_types::observe_event(&Arc<Mutex<WindowDataTracker>>, &Event)` | engine event | `Vec<BotEvent>` | `BotShared` |
| craft | `craft::observe_event(&Arc<Mutex<RecipeBook>>, &Event)` | engine event | nothing | `BotShared` |
| world | `world::observe_event(&Arc<RwLock<WorldState>>, &client, &mut LidCounts, &Event)` | engine event | `Vec<BotEvent>` | `BotShared` + bridge-local |
| misc + scoreboard | `misc::after_event(&client, &Mutex<ScoreboardState>, &BotEvent, &Sender)` | **translated** `BotEvent` | sends directly | ECS component + `BotShared` |
| chat | inside `translate` and `packet_side_events` | engine event | `Vec<BotEvent>` | `BotShared` |

Three naming schemes, four argument orders, two input types, and one
domain that sends on the channel itself instead of returning its events.

## The emission order

`D-bridge` in `docs/decisions.md` requires that derived events follow
their source packet. The current order for one engine event is:

1. All observers except misc run first, and their results are held.
2. `translate` produces the direct events. For each direct event, the
   bridge sends it (unless it is a `Packet` and packet events are off),
   then `misc::after_event` sends the misc and scoreboard events derived
   from it.
3. The held results are sent in a fixed order: movement, pathfinder,
   entity, windows, window types, world.

So misc events come right after their `Packet` and before the chat or
lifecycle events that `translate` also derived from the same packet,
while all other domains come after them. This order is real behavior
(a test or a user may depend on `Health` before `Spawn`), but nothing in
the code states it except the loop itself.

## Proposal: one contract

```rust
/// What every domain observer gets.
pub(crate) struct Observe<'a> {
    pub client: &'a mineflayer_engine::Client,
    pub shared: &'a BotShared,
    pub local: &'a mut BridgeLocal, // movement, path, lid, player-list state
}

// one shape for every domain
pub(crate) fn observe(cx: &mut Observe<'_>, event: &Event) -> Vec<BotEvent>;

// the bridge body
const OBSERVERS: &[fn(&mut Observe<'_>, &Event) -> Vec<BotEvent>] = &[
    misc::observe, scoreboard::observe, movement::observe, pathfinder::observe,
    entity::observe, windows::observe, window_types::observe, craft::observe,
    world::observe,
];
```

With this, the order lives in one constant, a new domain is one line,
and `misc` stops reading translated events. Scoreboard gets its own
entry, which matches where its state already lives (`BotShared`, decided
in D2). The bridge loop shrinks to: translate, run the list, send the
concatenation (filtering `Packet` when packet events are off).

Moving misc from "after each translated event" to an observer changes
where misc events land relative to `translate`'s derived events. Before
the change, write a unit test that pins today's order for the packets
that produce both kinds (`SetHealth` while dead gives `Spawn` and
`Health`; `Respawn` gives `Respawn` and misc nothing), then decide on
purpose whether to keep or change it. Upstream's `health.js` order is
the reference.

## The misc lock

`misc::handle_packet` (`src/misc/mod.rs:139`) runs for every
`BotEvent::Packet`. After the scoreboard check it always takes
`client.ecs.write()`, finds the client entity, inserts `MiscState` if it
is missing, and calls the pure reducer. Only one reducer arm
(`ResourcePackPush`) writes state; the others only build events from the
packet. `docs/performance.md` measured a peak of 8,751 events per second
with about 60% raw packets, so the bridge takes the exclusive ECS lock
about 5,000 times a second at peak, competing with the engine systems
that run on other threads.

Remedy: build the events from the packet without the lock, and take the
write lock only for `ResourcePackPush`. `MiscState` already exists from
`connect`, so the insert-if-missing branch can go too.

## The `BotEvent` enum

`src/bot/event.rs` holds one `#[non_exhaustive]` enum with 123 variants
and no `impl` blocks. One enum is a recorded decision (D-events), and the
size follows from mineflayer's event list, so this audit does not ask to
split it. Payloads carry engine types (`ChatPacket`, `PlayerInfo`,
`EntityRef`, `ClientboundGamePacket`). With the engine in-tree and
re-exported as `mineflayer::engine`, that is acceptable; the earlier
concern (third-party types in the public API) no longer applies.

## Bridge exit

`bridge::spawn` ends the client on three conditions (event channel
closed, all handles dropped, keepalive timeout leading to a disconnect)
and emits a fallback `End` when the engine stops without one. This logic
is correct and tested (A6 in the previous plan). After the observer
change it should move into its own function so `spawn` is the loop only.
