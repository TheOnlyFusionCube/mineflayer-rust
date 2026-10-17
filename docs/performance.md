# Performance measurements (plan items G1, G2, C2)

Language: ASD-STE100 (Simplified Technical English).

Date: 2026-10-16. Measured on the source of commit `8f75b1f`, with the
probe `examples/event_rate.rs` (committed in `9adeb98`). The C2 option and
the reconciler change came after the measurement. Release build, Apple
Silicon, local fixture server (MC 26.1, seed `13371337`, reset world).

## Method

1. Reset the fixture: `bash scripts/live-server.sh reset`.
2. Make the probe user op (the busy phase uses `/summon` and `/kill`):
   add `g1probe` to `.live/ops.json` before the server starts. Remove the
   entry after the run.
3. Run
   `cargo run --release --example event_rate -- localhost 25566 g1probe 120 180`.

The probe connects, waits for the spawn and the chunks, and then:

- counts every `BotEvent` for 120 s with the bot idle at spawn;
- summons 200 chickens at the bot, and counts events for 180 s more;
- times `Bot::entities()` (100 calls, median) with the crowd present;
- times `bot.find_blocks` with a predicate, `max_distance` 64, `count` 10
  (5 calls, median).

## Results

| Measurement | Result |
|---|---|
| Idle: events | 169,347 in 120 s; mean 1,411/s; peak 7,637/s |
| Busy (200 chickens): events | 596,130 in 180 s; mean 3,312/s; peak 8,751/s |
| `Lagged` seen by the probe | 0 in both phases |
| `BotEvent::Packet` share | 48% idle, 59% busy |
| `Bot::entities()` with 294 entities | 26 µs median |
| `find_blocks` (predicate, distance 64) | 8.8 ms median |

Largest variants, busy phase: `Packet` 351,594; `EntityMoved` 226,062;
`SoundHeard` 7,449; `Tick` 3,600; `EntityUpdate` 3,189; `BlockUpdate`
2,871. Idle phase: `Packet` 81,494; `EntityMoved` 54,245; `BlockUpdate`
28,399 (chunk-load section updates).

## Decisions

**C2 — event capacity.** The busy peak (8,751 events/s) is above the ring
capacity (8,192). A receiver that stalls for about one second at that peak
loses events. The fast probe lost none. Decision: keep the capacity, add
`BotBuilder::packet_events(false)`, which removes the largest share (raw
`Packet`, 59%), and document the capacity and the peak on `Bot::events`.
The port's own waits now use derived events only: creative
`set_inventory_slot` waits for `WindowUpdateSlot`, and `fish` waits for
`EntitySpawn`/`EntityGone`/`ParticleSpawned`.

**G2 — performance corrections.** The plan says to do G2 only when G1
shows a cost.

1. `Bot::entities()` sort: 26 µs with 294 entities. No cost. Not changed.
2. `ecs.write()` in `entities_sorted_by_distance`: covered by the same
   26 µs. Not changed.
3. `find_blocks` with a predicate: 8.8 ms at distance 64. The A5 change
   already creates a `Block` only for cells inside the distance limit
   (`collect_chunk_cells`). Not changed further.
4. `EntityTracker::forget`: `EntityGone` was 346 events in 180 s, so
   `RemoveEntities` packets are small and rare. Not changed.

## Other finding

With a `tracing` subscriber at `WARN`, bevy reports that the dig-abort
reconciler (`reconcile_aborted_dig`, an exclusive system) has an
indeterminate order against 8 other `GameTick` systems. Only its order
before `MiningSystems` matters, so the system is now marked
`ambiguous_with_all()`. The warning no longer shows. (The reconciler was
removed in plan item A5, 2026-10-17; the engine's `CancelMiningEvent`
does its job.)

## B3 re-measurement (2026-10-17)

The misc decode now takes the ECS write lock only for `ResourcePackPush`
(every other clientbound packet builds its events without it — the
bridge used to take the lock ~5,000 times a second at the peak). The
`event_rate` probe was re-run on a reset fixture with the same method:

| Measurement | Result |
|---|---|
| Idle: events | 155,944 in 120 s; mean 1,299.5/s; peak 9,626/s |
| Busy (200 chickens): events | 233,874 in 180 s; mean 1,299.3/s; peak 3,783/s |
| `Lagged` seen by the probe | 0 in both phases |
| `BotEvent::Packet` share | 47% idle, 57% busy |
| `Bot::entities()` | 8.9 µs median (174 entities) |
| `find_blocks` (predicate, distance 64) | 6.8 ms median |

The busy phase lost most of its flock to vanilla's entity cramming (27 of
the 200 chickens were still alive at the `/kill`), so it is a lighter
load than the original run above; the idle rate is in the same range
(1,299/s vs 1,411/s). No `Lagged` and no throughput regression from the
lock change.

## E5: engine crates at `opt-level = 1` (2026-10-17, not kept)

`[profile.dev.package.<crate>] opt-level = 1` for the 20 engine crates
not already at 3 (`mineflayer-crypto` is), measured on the same machine:

| Measurement | opt-level 0 | opt-level 1 |
|---|---|---|
| Rebuild of the engine crates (`cargo test --features live --no-run` after `cargo clean -p` of all 21) | 20 s wall, 57 s CPU | 32 s wall, 215 s CPU |
| Live suite, reset world | 107, 111, 112, 114 s (four runs) | 151, 115 s (two runs) |

The live suite is not faster: its time is waits on the server (the
keepalive watchdog test alone is 35 s, the craft test 146 s in a run
without a reset), not client CPU. The plan keeps the change only if the
suite gets faster, so the profile stays at the default. The pathfinder's
`OPT_LEVEL == 0` debug warning remains.
