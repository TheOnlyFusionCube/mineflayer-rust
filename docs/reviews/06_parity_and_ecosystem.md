# 06. Parity and ecosystem against upstream mineflayer

Findings F4, F8, and F10 in [`audit_summary.md`](audit_summary.md).
Baseline `ef06f74`; upstream `ref/mineflayer` at `f2b236a` (mineflayer
4.39.0, 2026-10-03).

## Side by side

| Dimension | mineflayer 4.39.0 | mineflayer-rs at `ef06f74` |
|---|---|---|
| API rows | the reference | 342 ported, 126 diverges, 47 wontfix, 11 additive (README) |
| Minecraft versions | 28, from 1.8.8 to 26.1 (`lib/version.js`) | 1 (26.1) |
| Extension | `bot.loadPlugin`, an npm ecosystem (pathfinder, pvp, collectblock, auto-eat, tool, statemachine, viewer, …) | none public; five plugin analogs inside the core crate |
| Examples | 48 scripts and 10 example projects | 3 ports (`echo`, `digger`, `chest`) and `event_rate` |
| Tests on a real server | 46 external test files, each run per version in a CI matrix of 28 versions | 14 live tests, one version, daily and by hand |
| Tests without a server | few | 254 unit tests (68% line coverage, `docs/coverage.md`) |
| Runtime | any Node.js >= 22 | one pinned nightly plus `-Znext-solver=no`, which dependents must copy |
| Distribution | npm, with types (`index.d.ts`) | not published (D-publish), no public repository |
| Docs | `docs/api.md`, FAQ, a docs site, translations | rustdoc (not hosted), `docs/api-parity.md` |
| Many bots in one process | one event loop for all bots | one OS thread, one tokio runtime, and one ECS world per bot (`src/bot/connection.rs:286-310`) |

The port will not match upstream on version count while the engine
targets one release; that is D-engine's cost, and the parity table
records it. On every other line the gap can be closed, and on three
lines the port can be better than upstream (see the last section).

## Capability gaps (F8)

These `diverges` and `wontfix` rows are missing features. The list is
ordered by how often a mineflayer bot uses the feature.

| Gap | Rows | Owner | Remedy |
|---|---|---|---|
| Chat components drop packets | `ChatMessage` (`api-parity.md:39`) | engine chat crate | see `05_engine_defects.md` |
| No plugin loader | `loadPlugin`, `loadPlugins`, `hasPlugin`, `options.plugins`, `loadInternalPlugins` | port | see `03_bridge_and_events.md` |
| Pathfinder never places blocks | `GoalPlaceBlock`, `isBuilding`, scaffolding fields | engine pathfinder | add place moves (pillar up, bridge) to the successors set, with a scaffold-block list in `Movements` |
| Pathfinder ignores six `Movements` fields | `Movements class` (`:554`) | engine pathfinder, port mapping | `can_open_doors` (a door move), `max_drop_down` (a fall cap in the descend moves), `allow_sprinting` (a flag to the executor), `dont_mine_under_falling_block` and `dont_create_flow` (break-cost filters); remove `allow_free_motion` or mark it `wontfix` |
| No block entities | `Block`, `updateSign`, sign text (`:35`, `:149`) | engine world | store block-entity NBT from chunk data and `block_entity_data` per chunk; expose `Block::block_entity()` and `Bot::sign_text()` |
| Physics is read-only and always on | `bot.physics`, `bot.physicsEnabled`, `options.physicsEnabled` | engine physics | a `PhysicsSettings` resource the physics systems read; disabling via the plugin group (see `05_engine_defects.md`) |
| Auto-respawn cannot be off | `options.respawn` | port | disable `AutoRespawnPlugin` in the plugin group |
| Resource packs: config phase unseen, denial ineffective | `acceptResourcePack`, `denyResourcePack`, `event('resourcePack')` | port | disable `AcceptResourcePacksPlugin`; a port system answers game and config pushes |
| Microsoft code callback | `options.onMsaCode` | engine auth | a callback parameter |
| Recipes only from the recipe book | `recipesFor`, `recipesAll`, `mcdata.recipes` | port data | a static recipe table from the vanilla data reports, generated at upgrade time, next to the book source |
| `login` fires once | `event('login')` (`:106`) | engine | emit `Event::Login` per `ClientboundLogin` |
| Language is `en_us` only | `ChatMessage` `lang` | engine language crate | accept a language map at build time |

## Adoption (F10)

### The toolchain

The README tells a dependent crate to copy `rust-toolchain.toml` and
`.cargo/config.toml` ("Use as a dependency"). That is a large ask: it
pins the dependent's whole workspace to one nightly, and the rustflag
changes the trait solver for every crate in it.

The engine uses seven feature gates. Each use is small.

| Gate | Crate | Use | Stable replacement |
|---|---|---|---|
| `trait_alias` | physics | 2 aliases (`IntLineConsumer`, `IndexConsumer`) | write the `FnMut` bound at each use, or a trait with a blanket impl |
| `min_specialization` | buf, inventory | 3 `default fn` sites | a marker trait or a macro for the specialized impls |
| `error_generic_member_access` | buf, client, protocol | thiserror `#[backtrace]` | remove the backtrace field, or capture `Backtrace` without the `provide` API |
| `generic_const_exprs` | core | const-generic bit sets | take the byte count as the const parameter |
| `type_changing_struct_update` | engine | one `..` struct update | write the fields out |

`-Znext-solver=no` is needed because the next solver cannot infer
`mineflayer-core`'s const generics. The `generic_const_exprs` change
above likely removes that need too; test it.

The cost is merge friction with the upstream engine, which uses nightly.
Each replacement is local and mechanical, so an upgrade merge can redo
it. Decide this before the publish question, because a nightly-only crate
on crates.io reaches few users.

### Publishing

D-publish keeps everything unpublished because publishing `mineflayer`
would first publish 21 engine crates. Two options exist. Publish all of
them under one version line, or fold the non-macro engine crates into one
`mineflayer-engine` crate (the five proc-macro crates must stay
separate), which gives seven crates. Folding conflicts with the upstream
directory layout that the merge process relies on
(`docs/version_upgrade.md`). This is a decision for the owner, not a code
change, and the plan lists it as one.

### Examples and docs

Upstream's examples are how most users learn the API. Port the ones that
exercise distinct API areas first: `inventory`, `sleeper`, `fisherman`,
`farmer`, `guard`, `trader`, `book`, `anvil`, `scoreboard`, `titles`,
`tab_complete`, `blockfinder`, `raycast`, `sound`, `multiple`,
`reconnector`, `pathfinder`, `collectblock`, `auto-eat`, and `attack`.
Each one also becomes a compile check (`cargo build --examples` is in the
gate) and, where the fixture allows, a live test.

## Where the port can be better than upstream

1. **Deterministic tests with no server.** Upstream's tests need a real
   Java server per version. The engine's `Simulation` harness lets the
   port test packet-in to `BotEvent`-out in milliseconds on every push
   (`07_tests_docs_tooling.md`). Chests, villagers, anvils, and enchanting
   tables, which the fixture cannot produce, become testable.
2. **Typed, exhaustive events.** With `#[non_exhaustive]` removed from
   the engine enums (F3) and plugin events moved to typed per-plugin
   channels (F4), the compiler shows every consumer that a new event
   affects. Upstream's string-keyed emitter cannot.
3. **Resource cost per bot.** A Rust bot should use less memory and CPU
   than a Node bot. Nobody has measured it. Measure resident memory and
   CPU per bot for 1, 10, and 100 bots against the fixture, for both
   projects, and publish the numbers in `docs/performance.md`. If the
   thread-per-bot model is the limit at 100 bots, an option to run many
   bots on one runtime comes next.
