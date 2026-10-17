# mineflayer-rs port plan

Port the mineflayer JS bot API (~96 methods, ~96 properties, ~96 events, 5 window classes) onto the engine (0.16) as a single Rust crate named `mineflayer`. Consumers are Rust authors porting mineflayer bot code; the unit of truth for coverage is `docs/api-parity.md`.

Done predicate: every api-parity.md row is `ported` or `diverges` (with reason), `scripts/verify.sh` green on main, and the example bots compile and run against a local offline-mode server.

## Architecture

- `Bot` is a cloneable handle wrapping `mineflayer_engine::Client` (already Arc + bevy ECS backed). `Bot::builder()` mirrors `createBot(options)`.
- Modules mirror mineflayer plugin domains: `chat`, `world`, `entity`, `movement`, `interact`, `pathfinder`, `inventory`, `craft`, `windows`, `misc` (scoreboard/teams/bossbar/title/particles/sound/bed/fishing/creative/settings).
- Events: `BotEvent` enum delivered over `tokio::sync::broadcast`. `bot.events()` returns a `Receiver`; `bot.on::<BotEventKind>()` style typed helpers where cheap. This is the EventEmitter analog: multi-subscriber, lag-tolerant.
- Event bridging strategy in two tiers: (1) translate the engine's `Event` enum; (2) for anything the engine does not lift (scoreboard, bossbar, particles, sound, per-position blockUpdate), decode raw clientbound packets via `Event::Packet` inside the bridge task. Tier 2 is how we reach parity without forking the engine.
- Properties that mineflayer exposes as plain fields map to methods or cheap ECS component reads (`bot.entity()` etc.). Divergences recorded in api-parity.md.
- Errors: `mineflayer::Error` (thiserror), one variant per domain, wrapping the engine error types.

## Constraints

- The engine via a crates.io dependency (0.16). Pinned git rev only if a needed API is unpublished — logged ruling first. Never vendored. (Superseded 2026-10-16: the engine is in-tree, see `docs/decisions.md`, D-engine.)
- No new deps beyond the engine/tokio/serde/thiserror/futures/regex (chat patterns) without a logged ruling.
- Repo is local-only on main. One unit = one worker = one commit series. No force-push, no history rewrite.
- Verify bar per unit: `cargo fmt --check`, `cargo clippy` zero new warnings, `cargo test` green, `cargo build` clean — via `scripts/verify.sh`.
- Tests never touch real Mojang auth or public servers. Live tier uses a local offline-mode vanilla server (`scripts/live-server.sh`, Java 21 available) behind `#[ignore]` + `live` feature.
- api-parity.md is the binding spec. Every unit updates its rows in the same commit as the code.
- Lock policy (ruling, B8): every crate lock is `parking_lot::Mutex`/`RwLock` — the family the engine already uses for the ECS lock. They do not poison, and the type has no `unwrap()`, so a panic while holding a lock cannot turn one bug into a permanently broken domain. `parking_lot` is a direct dependency (it was already in the graph via the engine). Lock order: a domain lock may be held while taking the ECS lock; never take a domain lock while holding the ECS lock (see `src/bot/mod.rs` next to `BotShared`).
- Log facade (ruling, C1): `tracing` is a direct dependency. The engine already logs through it, so it adds no new crate to the graph. The crate emits `debug!`/`warn!` only and never installs a subscriber. `tracing-subscriber` is a dev-dependency for the examples only. Never log chat text, tokens, or account data.

## Units

Pilot:

- **U0 scaffold**: cargo workspace, `mineflayer` crate, the engine dep, `Bot` + `BotEvent` skeleton, `scripts/verify.sh`, api-parity.md skeleton listing every API row as `pending`. Proves the brief template and verify recipe.

Phase 1 (core, sequential — everything depends on the bridge):

- **U1 lifecycle + event bridge**: `Bot::builder`, connect (offline + passthrough `Account`), `quit`, `respawn`, `end`; bridge task translating the engine `Event` to `BotEvent` (Login/Spawn/Respawn/Death/Kicked/End/Error/Chat/Tick); `username`, `version`, `entity`, `isAlive`, `waitForTicks`, `waitForChunksToLoad`. Live test: connect to fixture server, see Spawn, echo chat, disconnect.
- **U2 chat**: `chat`, `whisper`, `add_chat_pattern`/`add_chat_pattern_set`/`remove_chat_pattern` emitting `BotEvent::ChatPattern{name,matches}`, `await_message`, `tab_complete`; message/messagestr/actionBar/whisper events.
- **U3 world/blocks**: `block_at`, `find_block(s)`, `block_at_cursor`/`entity_at_cursor`/`can_see_block` (raycast), `update_sign`, chunk load/unload + blockUpdate events (tier-2 packet bridge), time/weather/game-state properties, `spawn_point`.
- **U4 entities/players**: `nearest_entity`, `find_player(s)`, `players` map, entity events via `Event::AddPlayer/RemovePlayer/UpdatePlayer` + packet bridge (equip/hurt/dead/swing), `attack`, `swing_arm`, `activate_entity(_at)`, `use_on`, `mount`/`dismount`/`move_vehicle`, `elytra_fly`, `vehicle`, `firework_rocket_duration`, explosion damage calc if feasible.
- **U5 movement/physics**: `set_control_state`/`clear_control_states`/`control_state`, `look`/`look_at` (async confirm), `move`/`forced_move` events, `jump_queued`/`jump_ticks`, `abilities`, `physics` knobs where the engine exposes them.

Phase 2 (interaction + navigation):

- **U6 interact**: `dig`/`stop_digging`/`dig_time`/`can_dig_block` + digging events, `place_block`/`place_entity`/`activate_block`, blockPlaced/entityPlaced.
- **U7 pathfinder**: `bot.pathfinder().goto`/`set_goal`/`stop`/`is_moving`, `Goal*` types mapping to the engine goals (GoalNear/GoalBlock/GoalXZ/GoalY/GoalInvert/GoalFollow/GoalCompositeAny/All), `Movements` config struct → the engine goto opts, `path_update`/`goal_reached` events.
- **U8 inventory core**: `inventory`, `held_item`, `quick_bar_slot`/`set_quick_bar_slot`, `equip`/`unequip`, `toss`/`toss_stack`, `consume`, `activate_item`/`deactivate_item`, `click_window`/`transfer`/`put_away`/`close_window`, `open_block`/`open_entity`/`open_container`, `simple_click`, Window base + Chest/Container deposit/withdraw/close + window events.
- **U9 typed windows**: Furnace, EnchantmentTable, Anvil, Villager (+`trade`), `write_book`/`sign_book`, `set_command_block`. Missing the engine internals are built over raw container clicks like mineflayer does.
- **U10 crafting**: `recipes_for`/`recipes_all`/`craft`.

Phase 3 (breadth, parallel-safe after U1 — disjoint modules):

- **U11 misc**: scoreboards/teams/bossbar/title (packet bridge + state structs), particles/sound/note events, `sleep`/`wake`/`is_a_bed`/`is_sleeping`, `fish()`, `creative` namespace, `settings`/`set_settings`, resource pack accept/deny + event, `get_explosion_damages`, `support_feature` (returns latest-version truth per standing order 11).
- **U12 ecosystem analogs**: `bot.collect_block()` (find+goto+dig loop), `bot.tool` → wrap the engine auto_tool, `bot.pvp` attack loop helper. Stretch: auto_eat.
- **U13 examples + docs**: port echo + digger + chest examples, README with parity table summary and usage.

## Sequencing

U0 (pilot) → U1 → U2..U5 sequential (core bridge consumers) → U6..U10 mostly sequential (inventory layers build) → U11..U13 parallelizable in worktrees (disjoint modules). Coordinator lands one unit at a time on main, in order.

## Verification per unit

`scripts/verify.sh` + unit-declared live test if it touches wire behavior. Reviewer gets brief + diff package + api-parity delta; verdicts: spec pass/fail + quality. Deferred minors go to the ledger, final review triages.

## Open risks

- ECS-event coverage for tier-2 packet bridge decoding (packet types must be public in mineflayer-protocol — verified during U1; fallback: file upstream issue, mark rows diverges).
  Status: closed. The packet types are public; the bridge decodes them, and `docs/api-parity.md` has zero `pending` rows.
- Villager/enchanting/anvil window semantics may need raw click choreography mineflayer implements itself; U9 briefs must read mineflayer's plugin source for the transaction sequence, not just api.md.
  Status: closed. `src/window_types/` implements the upstream click sequences; the parity rows record each divergence.
- MC version skew: the published engine is mc26.1, its repo HEAD is mc26.2. Live fixture jar must match the engine we link — resolve version in U0.
  Status: closed. The build uses the engine `ddef371` (`0.16.0+mc26.1`), and the fixture jar is 26.1 (`.live/version.txt`). See `docs/decisions.md` (D-pin).
