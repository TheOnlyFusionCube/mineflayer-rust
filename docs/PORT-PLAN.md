# mineflayer-rs port plan

Port the mineflayer JS bot API (~96 methods, ~96 properties, ~96 events, 5 window classes) onto azalea 0.16 as a single Rust crate named `mineflayer`. Consumers are Rust authors porting mineflayer bot code; the unit of truth for coverage is `docs/api-parity.md`.

Done predicate: every api-parity.md row is `ported` or `diverges` (with reason), `scripts/verify.sh` green on main, and the example bots compile and run against a local offline-mode server.

## Architecture

- `Bot` is a cloneable handle wrapping `azalea::Client` (already Arc + bevy ECS backed). `Bot::builder()` mirrors `createBot(options)`.
- Modules mirror mineflayer plugin domains: `chat`, `world`, `entity`, `movement`, `interact`, `pathfinder`, `inventory`, `craft`, `windows`, `misc` (scoreboard/teams/bossbar/title/particles/sound/bed/fishing/creative/settings).
- Events: `BotEvent` enum delivered over `tokio::sync::broadcast`. `bot.events()` returns a `Receiver`; `bot.on::<BotEventKind>()` style typed helpers where cheap. This is the EventEmitter analog: multi-subscriber, lag-tolerant.
- Event bridging strategy in two tiers: (1) translate azalea's `Event` enum; (2) for anything azalea does not lift (scoreboard, bossbar, particles, sound, per-position blockUpdate), decode raw clientbound packets via `Event::Packet` inside the bridge task. Tier 2 is how we reach parity without forking azalea.
- Properties that mineflayer exposes as plain fields map to methods or cheap ECS component reads (`bot.entity()` etc.). Divergences recorded in api-parity.md.
- Errors: `mineflayer::Error` (thiserror), one variant per domain, wrapping azalea error types.

## Constraints

- azalea via crates.io `azalea = "0.16"`. Pinned git rev only if a needed API is unpublished — logged ruling first. Never vendored.
- No new deps beyond azalea/tokio/serde/thiserror/futures/regex (chat patterns) without a logged ruling.
- Repo is local-only on main. One unit = one worker = one commit series. No force-push, no history rewrite.
- Verify bar per unit: `cargo fmt --check`, `cargo clippy` zero new warnings, `cargo test` green, `cargo build` clean — via `scripts/verify.sh`.
- Tests never touch real Mojang auth or public servers. Live tier uses a local offline-mode vanilla server (`scripts/live-server.sh`, Java 21 available) behind `#[ignore]` + `live` feature.
- api-parity.md is the binding spec. Every unit updates its rows in the same commit as the code.

## Units

Pilot:

- **U0 scaffold**: cargo workspace, `mineflayer` crate, azalea dep, `Bot` + `BotEvent` skeleton, `scripts/verify.sh`, api-parity.md skeleton listing every API row as `pending`. Proves the brief template and verify recipe.

Phase 1 (core, sequential — everything depends on the bridge):

- **U1 lifecycle + event bridge**: `Bot::builder`, connect (offline + passthrough `Account`), `quit`, `respawn`, `end`; bridge task translating azalea `Event` to `BotEvent` (Login/Spawn/Respawn/Death/Kicked/End/Error/Chat/Tick); `username`, `version`, `entity`, `isAlive`, `waitForTicks`, `waitForChunksToLoad`. Live test: connect to fixture server, see Spawn, echo chat, disconnect.
- **U2 chat**: `chat`, `whisper`, `add_chat_pattern`/`add_chat_pattern_set`/`remove_chat_pattern` emitting `BotEvent::ChatPattern{name,matches}`, `await_message`, `tab_complete`; message/messagestr/actionBar/whisper events.
- **U3 world/blocks**: `block_at`, `find_block(s)`, `block_at_cursor`/`entity_at_cursor`/`can_see_block` (raycast), `update_sign`, chunk load/unload + blockUpdate events (tier-2 packet bridge), time/weather/game-state properties, `spawn_point`.
- **U4 entities/players**: `nearest_entity`, `find_player(s)`, `players` map, entity events via `Event::AddPlayer/RemovePlayer/UpdatePlayer` + packet bridge (equip/hurt/dead/swing), `attack`, `swing_arm`, `activate_entity(_at)`, `use_on`, `mount`/`dismount`/`move_vehicle`, `elytra_fly`, `vehicle`, `firework_rocket_duration`, explosion damage calc if feasible.
- **U5 movement/physics**: `set_control_state`/`clear_control_states`/`control_state`, `look`/`look_at` (async confirm), `move`/`forced_move` events, `jump_queued`/`jump_ticks`, `abilities`, `physics` knobs where azalea exposes them.

Phase 2 (interaction + navigation):

- **U6 interact**: `dig`/`stop_digging`/`dig_time`/`can_dig_block` + digging events, `place_block`/`place_entity`/`activate_block`, blockPlaced/entityPlaced.
- **U7 pathfinder**: `bot.pathfinder().goto`/`set_goal`/`stop`/`is_moving`, `Goal*` types mapping to azalea goals (GoalNear/GoalBlock/GoalXZ/GoalY/GoalInvert/GoalFollow/GoalCompositeAny/All), `Movements` config struct → azalea goto opts, `path_update`/`goal_reached` events.
- **U8 inventory core**: `inventory`, `held_item`, `quick_bar_slot`/`set_quick_bar_slot`, `equip`/`unequip`, `toss`/`toss_stack`, `consume`, `activate_item`/`deactivate_item`, `click_window`/`transfer`/`put_away`/`close_window`, `open_block`/`open_entity`/`open_container`, `simple_click`, Window base + Chest/Container deposit/withdraw/close + window events.
- **U9 typed windows**: Furnace, EnchantmentTable, Anvil, Villager (+`trade`), `write_book`/`sign_book`, `set_command_block`. Missing azalea internals are built over raw container clicks like mineflayer does.
- **U10 crafting**: `recipes_for`/`recipes_all`/`craft`.

Phase 3 (breadth, parallel-safe after U1 — disjoint modules):

- **U11 misc**: scoreboards/teams/bossbar/title (packet bridge + state structs), particles/sound/note events, `sleep`/`wake`/`is_a_bed`/`is_sleeping`, `fish()`, `creative` namespace, `settings`/`set_settings`, resource pack accept/deny + event, `get_explosion_damages`, `support_feature` (returns latest-version truth per standing order 11).
- **U12 ecosystem analogs**: `bot.collect_block()` (find+goto+dig loop), `bot.tool` → wrap azalea auto_tool, `bot.pvp` attack loop helper. Stretch: auto_eat.
- **U13 examples + docs**: port echo + digger + chest examples, README with parity table summary and usage.

## Sequencing

U0 (pilot) → U1 → U2..U5 sequential (core bridge consumers) → U6..U10 mostly sequential (inventory layers build) → U11..U13 parallelizable in worktrees (disjoint modules). Coordinator lands one unit at a time on main, in order.

## Verification per unit

`scripts/verify.sh` + unit-declared live test if it touches wire behavior. Reviewer gets brief + diff package + api-parity delta; verdicts: spec pass/fail + quality. Deferred minors go to the ledger, final review triages.

## Open risks

- ECS-event coverage for tier-2 packet bridge decoding (packet types must be public in azalea-protocol — verified during U1; fallback: file upstream issue, mark rows diverges).
- Villager/enchanting/anvil window semantics may need raw click choreography mineflayer implements itself; U9 briefs must read mineflayer's plugin source for the transaction sequence, not just api.md.
- MC version skew: crates.io azalea is mc26.1, repo HEAD is mc26.2. Live fixture jar must match the azalea we link — resolve version in U0.
