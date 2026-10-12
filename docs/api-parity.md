# mineflayer -> mineflayer-rs API parity table

Binding spec: every public API row from `docs/surveys/mineflayer-api.md`, grouped by domain.
Status ∈ `pending` (not ported), `ported`, `diverges` (intentional semantic difference), `wontfix`, `additive` (Rust surface beyond the upstream inventory).

Global divergence (standing order 11): **azalea tracks a single MC version** (crates.io azalea `0.16.0+mc26.1` -> MC `26.1`).
Every version-selection row is `diverges` — single-version, latest-only. Noted once here, not per row.

Naming: Rust `snake_case`, `Promise<T>` -> `async fn -> Result<T>` (or a stream/event where noted).
Events become `BotEvent` variants or per-domain event enums; payloads use azalea types.

## Module exports (`require('mineflayer')`)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `createBot(options)` | module | `Bot::builder() -> BotBuilder; BotBuilder::connect() -> Result<Bot>` | ported | builder style; connect runs bot on a dedicated thread (azalea LocalSet) |
| `Location` | module | `mineflayer::Location::new(Vec3)` | ported | same floored/block_point/chunk_corner/block_index/biome_block_index/chunk_y_index formulas (upstream lib/location.js); `rem_euclid(16)` matches upstream's `modulus` on negatives; upstream's Vec3 fields are `BlockPos` here (integer-identical — upstream floors them anyway) |
| `Painting` | module | `Painting::from_entity(&EntityRef) -> Option<Painting>` | diverges | `{id, position, variant, direction}` read from painting-entity metadata — `spawn_entity_painting` packet no longer exists in mc26; `name` is the `data::PaintingVariant` handle (resolve title via `client.resolve_registry_key`), and upstream's `block.painting` position index isn't ported |
| `ScoreBoard (class)` | module | `mineflayer::ScoreBoard` | ported | see Scoreboard section |
| `BossBar (class)` | module | `mineflayer::BossBar` | ported | see Scoreboard section |
| `Particle (class)` | module | `mineflayer::Particle` | ported | see Particles section |
| `testedVersions` | module | `n/a` | diverges | azalea tracks latest MC only (standing order 11); no multi-version list |
| `latestSupportedVersion` | module | `const MC_VERSION: &str` | diverges | single-version: equals the azalea-targeted MC version |
| `oldestSupportedVersion` | module | `n/a` | diverges | single-version: same as latest |
| `supportFeature(feature, version)` | module | `n/a` | wontfix | upstream feature-support lookup has no analog; not implemented |
| `mcdata.blocks` | module | `mcdata::BlockKind` | diverges | closed enum, not a lookup table: `Registry::from_u32`/`to_u32` = id index, `to_str`/`FromStr`/`TryFrom<&Identifier>` = name lookups; per-block data lives on `BlockTrait::behavior` |
| `mcdata.items` | module | `mcdata::ItemKind` | diverges | same enum surface as blocks; per-item data is on `ItemStack` data components, not a static record |
| `mcdata.materials` | module | `n/a` | wontfix | minecraft-data's material→tool-multiplier table models a pre-1.20.5 era; in mc26 dig speed is per-item `minecraft:tool` data-component rules over block tag sets — `azalea::entity::mining::get_mine_progress` already implements that path |
| `mcdata.recipes` | module | recipe book bridge (`c_recipe_book_add`/`remove`) | diverges | book-scoped server-unlocked recipes, not a static mcdata table; tag ingredients resolve via azalea's static item tags; azalea's `ItemStack` slot display decodes kind/count swapped — un-swapped at conversion |
| `mcdata.instruments` | module | `enum Instrument` (misc.rs, re-exported) | ported | 16 note-block instruments; `From<u8>` action-id decode; used by `NoteHeard` |
| `mcdata.biomes` | module | `mcdata::Biome` + `mcdata::BiomeKey` | diverges | protocol-id handle into the server's `worldgen/biome` registry (data-driven since 1.18 — no compile-time table); resolve name via `client.resolve_registry_key`, element NBT under `RegistryHolder::extra` |
| `mcdata.entities` | module | `mcdata::EntityKind` | diverges | closed enum, not a lookup table (same `Registry`/name surface as BlockKind) |
| `Vec3 (prismarine)` | module | `azalea::Vec3` + `mineflayer::Vec3Ext` trait | ported | Vec3Ext covers node-vec3's missing surface (offset/floored/modulus/unit/to_array/xy/yz/set/update/…); gaps: mutating `scale`/`normalize`/`add`/`subtract`/`multiply`/`divide` (names shadowed by azalea's inherent non-mutating versions — `*=`/`+=`/ops), `equals(other,error)` tolerance arg, `xz()` returns Vec3 not the `[x,z]` pair |
| `Entity (prismarine-entity)` | module | `azalea::EntityRef` + `EntityExt` trait | diverges | EntityExt covers look/velocity/onGround/username/skin only; uuid/kind/attributes/effects/metadata need raw `get_component`, equipment needs `bot.equipment` |
| `Block (prismarine-block)` | module | `mineflayer::world::Block { pos, state, world }` | diverges | name/kind/state_id/bounding_box/diggable/property/behavior ported; `name()` is namespaced (`"minecraft:gravel"`, upstream `"gravel"` — exposed by examples/digger.rs; `Item::name` does strip); no blockEntity/getSignText (azalea has no block-entity storage — U11) |
| `Biome (prismarine-biome)` | module | `mineflayer::Biome` (= `azalea::registry::data::Biome`) | diverges | bare protocol-id handle — upstream objects carry `name`/`displayName`/`temperature`/`rainfall`; those fields live in server-sent registry NBT (`RegistryHolder.extra` under `worldgen/biome`), and the key resolves via `client.resolve_registry_key` — no field access on the handle itself (same gap as `mcdata.biomes`) |
| `Item (prismarine-item)` | module | `mineflayer::Item { stack, slot }` | ported | kind/name/display_name/count/metadata/durability/component_patch + same_item; empty stack → `None` like upstream `null`; slot -1 = cursor |
| `Recipe (prismarine-recipe)` | module | `craft::Recipe` (+ `Ingredient`/`RecipeItem`/`RecipeResult`/`DeltaEntry`) | diverges | fields ported; ingredients model tag alternatives as `any_of` sets instead of per-kind recipe expansion |
| `ChatMessage (prismarine-chat)` | module | `mineflayer::ChatMessage` (wraps `azalea::FormattedText`) | diverges | parse/from_notch/to_string/to_ansi/to_html + text/translate/with/extra/style accessors; missing upstream's `lang` overrides (en_us only), indexed `getText`/`length`, `toMotd`, `MessageBuilder`; `score`/`selector`/`keybind`/`nbt`/`object` components fail to parse entirely (azalea-chat deserialize errors where prismarine-chat accepts) |
| `Window (prismarine-windows)` | module | `mineflayer::Window` (Arc over `ContainerHandleRef`) | ported | cheap cloneable shared handle + explicit idempotent `close()` like upstream; `ContainerHandle` drop-close intentionally not used; see Window classes |

## `createBot(options)` fields

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `options.username` | lifecycle | `BotBuilder::username(impl Into<String>)` | ported | default 'Player' |
| `options.port` | lifecycle | `BotBuilder::port(u16)` | ported | default 25565 |
| `options.password` | lifecycle | `n/a` | diverges | Mojang password auth is dead upstream too; use `Auth::Microsoft` |
| `options.host` | lifecycle | `BotBuilder::host(impl Into<String>)` | ported | default localhost |
| `options.version` | lifecycle | `n/a` | diverges | azalea tracks latest MC only; no version selection |
| `options.auth` | lifecycle | `BotBuilder::auth(Auth)` | ported | `Auth::{Offline, Microsoft, Account}` |
| `options.clientToken` | lifecycle | `n/a` | wontfix | n/a — generated inside Account; no upstream hook |
| `options.accessToken` | lifecycle | `Auth::Account(Account::with_microsoft_access_token(..))` | ported | prebuilt account passthrough |
| `options.logErrors` | lifecycle | `n/a` | wontfix | no logging facade in the dep set — errors surface only as `BotEvent::Error` (upstream's `logErrors:false` state, permanently) |
| `options.hideErrors` | lifecycle | `n/a` | wontfix | same — nothing is ever printed to console; `_warn` has no analog |
| `options.keepAlive` | lifecycle | `n/a` | wontfix | n/a — azalea owns keepalive; `Event::KeepAlive` dropped by the bridge |
| `options.checkTimeoutInterval` | lifecycle | `BotBuilder::check_timeout_interval(Duration)` | ported | keepalive watchdog in the bridge: armed/re-armed per `Event::KeepAlive` like upstream's per-keepalive `setTimeout`; on expiry emits `BotEvent::Error("client timed out after N milliseconds")` then disconnects → `End("keepAliveError")`; default 30s |
| `options.loadInternalPlugins` | lifecycle | `n/a` | wontfix | n/a — compile-time plugin model |
| `options.storageBuilder` | lifecycle | `n/a` | wontfix | n/a — anvil provider out of scope |
| `options.client` | lifecycle | `Bot::new(client)` | ported | doc-hidden escape hatch; no event bridge on prebuilt clients |
| `options.brand` | lifecycle | `BotBuilder::brand(impl Into<String>)` | ported | `minecraft:brand` custom payload sent on `Event::Login` — upstream's login-packet timing; azalea also sends `"vanilla"` during config phase, and the server keeps the last brand received, so ours wins |
| `options.respawn` | lifecycle | `n/a` | diverges | azalea auto-respawns unconditionally; no toggle exposed |
| `options.plugins` | lifecycle | `n/a` | wontfix | no runtime plugin loader — compile-time plugin model (same as `loadInternalPlugins`/`loadPlugin*`/`hasPlugin`) |
| `options.physicsEnabled` | lifecycle | `n/a` | wontfix | azalea's PhysicsPlugin is baked into `Client::join`'s default plugins — no disable seam; `bot.physics_enabled()` reports always-true |
| `options.chat (settings passthrough)` | lifecycle | `BotBuilder::chat(ChatVisibility)` | ported | `'enabled'`/`'commandsOnly'`/`'disabled'` → `ChatVisibility::Full`/`System`/`Hidden`; merged into the `ClientInformation` sent on Init |
| `options.colorsEnabled` | lifecycle | `BotBuilder::colors_enabled(bool)` | ported | → `ClientInformation::chat_colors` on Init |
| `options.viewDistance` | lifecycle | `BotBuilder::view_distance(impl Into<ViewDistance>)` | ported | applied on `Event::Init` so login already carries it; `ViewDistance` covers upstream's `'far'`/`'normal'`/`'short'`/`'tiny'` presets (12/10/8/6) plus raw counts |
| `options.difficulty` | lifecycle | `BotBuilder::difficulty(u8)` | ported | stored client-side (`MiscState.difficulty`, default 2) → `bot.settings().difficulty`; never sent, server-authoritative — same as upstream |
| `options.skinParts` | lifecycle | `BotBuilder::skin_parts(ModelCustomization)` | ported | → `ClientInformation::model_customization` on Init |
| `options.enableTextFiltering` | lifecycle | `BotBuilder::enable_text_filtering(bool)` | ported | → `ClientInformation::text_filtering_enabled` on Init |
| `options.enableServerListing` | lifecycle | `BotBuilder::enable_server_listing(bool)` | diverges | upstream's initializer is `options.enableServerListing \|\| true` — the flag is dead and the packet always sends `true`; ours applies the flag, default unset = azalea's `false` |
| `options.particleStatus` | lifecycle | `BotBuilder::particle_status(ParticleStatus)` | diverges | upstream parses the option but hardcodes `'all'` into `bot.settings` — the option is ignored, `'all'` goes on the wire; ours lands the option on the wire |
| `options.chatLengthLimit` | lifecycle | `BotBuilder::chat_length_limit(usize)` | ported | `chat`/`whisper` split at the limit (default 256); counts Unicode scalar `chars` like the rest of this port, not upstream's UTF-16 `substring` |
| `options.defaultChatPatterns` | lifecycle | `BotBuilder::default_chat_patterns(bool)` | diverges | `false` suppresses `BotEvent::Whisper` (the `is_whisper` heuristic standing in for upstream's deprecated default whisper patterns); `BotEvent::Chat` stays — it's the raw packet stream, already `diverges`, not a pattern product |
| `options.mainHand` | lifecycle | `BotBuilder::main_hand(HumanoidArm)` | ported | → `ClientInformation::main_hand` on Init |
| `options.maxCatchupTicks` | lifecycle | `n/a` | wontfix | upstream bounds its own physics catch-up loop; azalea-physics advances once per game tick — no catch-up accumulator exists to bound |
| `options ClientOptions passthrough` | lifecycle | `BotBuilder::proxy(Proxy)` | diverges | SOCKS5 proxy via azalea's `join_with_proxy` covers `agent`; `realms`/`profilesFolder`/`session`/`stream`/`connect`-callback have no azalea join hook — those remain wontfix per-knob |
| `options.skipValidation` | lifecycle | `n/a` | wontfix | minecraft-protocol ClientOption for skipping session-server validation; azalea has no validation layer to skip — `Auth::Offline` already covers the dev-server case |
| `options.authTitle` | lifecycle | `n/a` | wontfix | the Azure app title shown during Microsoft auth; azalea-auth hardcodes its client id/flow (`interactive_get_ms_auth_token`) — no title knob |
| `options.fetchProfile` | lifecycle | always-on | diverges | upstream's opt-in full-profile fetch (skins/capes); azalea always fetches the full game profile during auth — no opt-out needed or offered |
| `options.onMsaCode` | lifecycle | `n/a` | wontfix | device-code callback for custom auth UIs; azalea-auth prints `verification_uri?otc=code` to stdout inside `interactive_get_ms_auth_token` — no interception seam |

Upstream's 42 internal plugins (survey: `lib/plugins/*`) are tracked in aggregate — the compile-time-plugin rows above and `bot.loadPlugin`/`loadPlugins`/`hasPlugin` in lifecycle cover them; there are no per-plugin rows by design.

## Bot lifecycle & core

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.end(reason?)` | lifecycle | `bot.end(Option<&str>)` + drop last `Bot` to shut down | ported | End carries reason (default 'socketClosed') |
| `bot.quit(reason?)` | lifecycle | `bot.quit(Option<&str>)` | ported | graceful disconnect, default 'disconnect.quitting' |
| `bot.respawn()` | lifecycle | `bot.respawn()` | ported | no-op while alive; s_client_command PerformRespawn |
| `bot.connect(options)` | lifecycle | `BotBuilder::connect() -> Result<Bot>` | ported | builder form (typed-only upstream anyway) |
| `bot.loadPlugin(plugin)` | lifecycle | `n/a` | wontfix | n/a — compile-time plugin model |
| `bot.loadPlugins(plugins)` | lifecycle | `n/a` | wontfix | n/a — compile-time plugin model |
| `bot.hasPlugin(plugin)` | lifecycle | `n/a` | wontfix | n/a — compile-time plugin model |
| `bot.supportFeature(name)` | lifecycle | `n/a` | wontfix | upstream feature-support lookup has no analog; not implemented |
| `bot.username` | lifecycle | `bot.username() -> String` | ported | GameProfileComponent name; '' before login |
| `bot.version` | lifecycle | `bot.version() -> &'static str` | diverges | returns the single supported MC version (azalea mc26.1) |
| `bot.protocolVersion` | lifecycle | `bot.protocol_version() -> i32` | ported | protocol number of the single supported version (775) |
| `bot.majorVersion` | lifecycle | `bot.major_version() -> &'static str` | ported | same as `version()` — single-version port |
| `bot.registry` | lifecycle | `bot.registry() -> RegistryHolder` | ported | azalea dynamic registries snapshot |
| `bot._client` | lifecycle | `bot.client() -> &azalea::Client` | ported | unstable upstream; public escape hatch here |
| `bot._warn(...)` | lifecycle | `n/a` | wontfix | n/a — tracing/logging, not an API |
| `bot._getDimensionName() (internal)` | lifecycle | `bot.dimension() -> Option<WorldName>` | ported | upstream internal helper reads `bot.game.dimension`; ours is the public `dimension()` getter (WorldName = the dimension identifier) |
| `event('inject_allowed')` | lifecycle | `BotEvent::Init` | ported | azalea Event::Init analog; the first `bot.events()` receiver is subscribed before the bridge starts, so an Init queued during `connect` can't be missed |
| `event('login')` | lifecycle | `BotEvent::Login` | diverges | azalea `Event::Login` fires once per entity lifetime (never refires in 0.16) — a mid-session `ClientboundLogin` (proxy/config-cycle relogin) does not re-emit it; upstream emits per login packet |
| `event('spawn')` | lifecycle | `BotEvent::Spawn` | ported | initial `Event::Spawn`; post-respawn 'spawn' from the first `c_set_health` with health > 0 while dead — upstream's `isAlive` re-arm (azalea Spawn never refires in 0.16) |
| `event('respawn')` | lifecycle | `BotEvent::Respawn` | ported | emitted per `c_respawn` packet — death respawn and dimension change alike, like upstream's `respawn` packet handler |
| `event('connect') (undoc)` | lifecycle | `BotEvent::Connected` | diverges | upstream fires on raw socket connect (pre-login); azalea surfaces no socket-level event — emitted right before the first `Login` (earliest wire-up proof), once per connection (azalea Login is also once-only, so the edge is exactly once) |
| `event('kicked')` | lifecycle | `BotEvent::Kicked(String)` | diverges | azalea folds kick into `Disconnect(reason)`; we emit Kicked then End; no loggedIn flag |
| `event('end')` | lifecycle | `BotEvent::End(String)` | ported | exactly-once; reason = quit arg / kick / 'socketClosed' |
| `event('error')` | lifecycle | `BotEvent::Error(String)` | ported | currently: connection failures only |

## Chat / messaging

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.chat(message)` | chat | `bot.chat(&str) -> Result<()>` | ported | /-prefixed -> command; >256 split like upstream (newline-aware, header counts); pre-play errors keep upstream's two cases — `bot.chat() called before the client entered the play state…` vs `…after the client disconnected before entering the play state (<endReason>)` when the bridge recorded a pre-play `end` |
| `bot.whisper(user, msg)` | chat | `bot.whisper(&str, &str) -> Result<()>` | diverges | `/tell user ` header counts against the 256 limit; upstream uses /tell only (no /msg fallback); same two-case pre-play errors as `chat`; header ≥ limit clamps to 1-char chunks — upstream's `i += lengthLimit ≤ 0` never advances and loops forever (upstream bug we deliberately don't reproduce) |
| `bot.chatAddPattern(...) (dep)` | chat | `n/a` | wontfix | deprecated upstream alias of addChatPattern; use `add_chat_pattern` |
| `bot.addChatPattern(name, re, opts)` | chat | `bot.add_chat_pattern(&str, Regex, ChatPatternOpts) -> usize` | ported | `opts{repeat=true,parse=false}`; upstream `deprecated` opt skipped (covered by BotEvent::Whisper) |
| `bot.addChatPatternSet(name, res, opts)` | chat | `bot.add_chat_pattern_set(&str, Vec<Regex>, ChatPatternOpts) -> usize` | diverges | sequential pattern matching ported; returns the real id — upstream's set return is off-by-one vs its removal key |
| `bot.removeChatPattern(name\|id)` | chat | `bot.remove_chat_pattern(impl Into<ChatPatternKey>)` | ported | `ChatPatternKey::{Id,Name}`; name removes all sets with it |
| `bot.awaitMessage(...args)` | chat | `bot.await_message(impl IntoIterator<Item = impl Into<MessageMatcher>>) -> Result<String>` | diverges | `MessageMatcher::{Exact,OneOf,Regex}` vs JS arg coercion; resolves messagestr; 20s default timeout; no trailing-timeout arg |
| `bot.tabComplete(str, ...)` | chat | `bot.tab_complete(&str) -> Result<Vec<String>>` | diverges | s_command_suggestion via write_packet + id-correlated reply registry (azalea drops c_command_suggestions); assumeCommand/sendBlockInSight have no mc26 wire fields — dropped; include `/` in text for command completion |
| `bot.chatPatterns` | chat | `bot.chat_patterns() -> Vec<ChatPattern>` | diverges | owned snapshot, not a live view; fields `{id,name,patterns,repeat,parse}` vs upstream `{pattern,type,description}` |
| `bot.tablist` | chat | `bot.tablist() -> TabList` | ported | `TabList{header,footer: FormattedText}`; bridge stores c_tab_list (dropped by azalea) |
| `event('chat')` | chat | `BotEvent::Chat(azalea::chat::ChatPacket)` | diverges | raw packet for every incoming chat (upstream 'chat' fires only via the legacy default pattern); derived family: Message/MessageStr/Whisper/ChatPattern |
| `event('whisper')` | chat | `BotEvent::Whisper(ChatPacket)` | ported | `ChatPacket::is_whisper` (signed incoming-whisper key + legacy text shapes) vs upstream's deprecated default patterns |
| `event('actionBar')` | chat | `BotEvent::ActionBar(FormattedText)` | ported | from c_set_action_bar_text and overlay system-chat (azalea drops the packet; bridge decodes) |
| `event('message')` | chat | `BotEvent::Message{packet, position}` | diverges | sender UUID via `packet.sender_uuid()` (no `verified` flag — azalea doesn't verify signatures); disguised chat counts as 'chat' position |
| `event('messagestr')` | chat | `BotEvent::MessageStr{message, position, packet}` | ported | `packet.message().to_string()` render; drives patterns + await_message like upstream |
| `event('unmatchedMessage')` | chat | `n/a` | wontfix | typed-only upstream, never emitted |
| `event('chat:{name}')` | chat | `BotEvent::ChatPattern{name, matches}` | ported | `matches: ChatPatternMatch::{Messages,Groups}` — capture groups when `parse` |

## World / blocks

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.blockAt(point)` | world | `bot.block_at(pos) -> Option<Block>` | ported | None when the chunk isn't loaded |
| `bot.waitForChunksToLoad()` | world | `bot.wait_for_chunks_to_load() -> Result<()>` | ported | resolves when the 5×5 chunk area around the bot is loaded; 10s timeout |
| `bot.blockInSight() (dep)` | world | `bot.block_in_sight() -> Option<Block>` | ported | `#[deprecated]`; = block_at_cursor(80) |
| `bot.blockAtCursor(maxDist, matcher)` | world | `bot.block_at_cursor(max_distance, matcher) -> Option<Block>` | ported | own DDA raycast — upstream WorldSync.raycast semantics: matcher accepts on voxel entry, no-matcher clips collision shapes (block.shapes); max default 256 |
| `bot.entityAtCursor(maxDist)` | world | `bot.entity_at_cursor(max_distance) -> Option<EntityRef>` | diverges | uses azalea's cached crosshair HitResult (pick-plugin range); max_distance only caps the result |
| `bot.blockAtEntityCursor(e, max, m)` | world | `bot.block_at_entity_cursor(&entity, max_distance, matcher) -> Option<Block>` | diverges | uses eye height, upstream uses entity.height |
| `bot.canSeeBlock(block)` | world | `bot.can_see_block(&block) -> bool` | ported | eye->center ray, range = distance to corner; matcher = collision-shape clip OR voxel entry into target (upstream blocks.js match) |
| `bot.findBlocks(options)` | world | `bot.find_blocks(FindBlocksOpts) -> Vec<BlockPos>` | ported | euclid closest-first; defaults max_distance=16, count=1; falsy coercion like upstream's `\|\|` — `max_distance: 0`/`NaN` → 16, `count: 0` → 1 (not `[]`); chunk-layer early exit |
| `bot.findBlock(options)` | world | `bot.find_block(FindBlocksOpts) -> Option<Block>` | ported | — |
| `bot.updateSign(block, text, back)` | world | `bot.update_sign(&block, text, back) -> Result<()>` | diverges | write-only: sends s_sign_update (front/back); sign text unreadable (no block-entity storage — U11) |
| `bot.world` | world | `bot.world() -> Option<WorldRef>` | diverges | WorldRef wraps the client (parking_lot lock type isn't nameable); get_block_state/block_at/raycast/find_blocks/set_block_state |
| `bot.spawnPoint` | world | `bot.spawn_point() -> BlockPos` | ported | c_set_default_spawn_position decoded in packet bridge |
| `bot._updateBlockState (internal)` | world | `n/a` | wontfix | azalea applies block updates to the world itself |
| `bot._getBlockAtEyeLevel (internal)` | world | `n/a` | wontfix | internal helper; eye raycasts go through `block_at_cursor` |
| `event('blockUpdate')` | world | `BotEvent::BlockUpdate { old, new }` | diverges | c_block_update + c_section_blocks_update decoded; `old` comes from a bounded per-position update history (`WorldState.last_block_updates`, 2¹⁶ entries, cleared on login/respawn and per-position on chunk unload) — the bridge decodes the packet only after azalea's `Update` pass applied the change to the live world, so a live read can't answer; a never-before-updated position still reports `old == new` (the post-apply read), `None` when unloaded |
| `event('blockUpdate:(x,y,z)')` | world | `match` on `BotEvent::BlockUpdate`'s `new.pos` | diverges | no per-position emitter; subscribers filter |
| `event('blockPlaced')` | world | `BotEvent::BlockPlaced{old,new}` | ported | emitted by `place_block` after the server confirms |
| `event('blockEntityData')` | world | `BotEvent::BlockEntityData(ClientboundBlockEntityData)` | ported | whole packet forwarded (pos + kind + NBT) |
| `event('signOpen')` | world | `BotEvent::SignOpen { block, is_front_text }` | ported | c_open_sign_editor decoded |
| `event('chunkColumnLoad')` | world | `BotEvent::ChunkColumnLoad(ChunkPos)` | ported | azalea Event::ReceiveChunk |
| `event('chunkColumnUnload')` | world | `BotEvent::ChunkUnload(ChunkPos)` | ported | c_forget_level_chunk decoded |
| `event('pistonMove')` | world | `BotEvent::PistonMove { block, is_pulling, direction }` | ported | c_block_event; direction is the raw facing byte like upstream |
| `event('chestLidMove')` | world | `BotEvent::ChestLidMove { block, open_count, second }` | ported | c_block_event + openCountByPos dedup port |
| `event('blockBreakProgressObserved')` | world | `BotEvent::BlockBreakProgress { block, stage, entity }` | ported | c_block_destruction stage<=9; entity via entity_by_minecraft_id |
| `event('blockBreakProgressEnd')` | world | `BotEvent::BlockBreakEnd { block, entity }` | ported | c_block_destruction stage>9 |

## Entities / players / combat

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.nearestEntity(match?)` | entity | `bot.nearest_entity(pred) -> Option<EntityRef>` | ported | skips bot.entity like upstream; nearest_entities_by + post-filter |
| `bot.findPlayer(filter) (undoc)` | entity | `bot.find_player(f) -> Option<EntityRef>` | diverges | upstream's string-filter quirk (returns array when >1 match) dropped; always Option |
| `bot.findPlayers(filter)` | entity | `bot.find_players(f) -> Vec<EntityRef>` | ported | `PlayerFilter::{Any,Username,Matches,Predicate}` mirrors string/RegExp/fn/null |
| `bot.attack(entity, swing)` | entity | `bot.attack(e, swing)` | ported | swing=true routes through azalea's attack-with-swing; false sends bare s_attack |
| `bot.swingArm(hand, showHand)` | entity | `bot.swing_arm(hand)` | diverges | public call is upstream's `showHand` default `true` — `s_swing` always carries a hand; upstream's `showHand:false` omits the field → wire default `hand:0` (main), which `PlaceOpts::show_hand:false` mirrors by sending a main-hand swing (place paths only) |
| `bot.activateEntity(e)` | entity | `bot.activate_entity(e)` | diverges | `look_at(entity.position + 1)` before interact like upstream's `lookAt(...offset(0,1,0))` (now async); errors on a positionless entity like upstream's throw; azalea double-sends `s_interact` (attack+interact arms) vs upstream's one |
| `bot.activateEntityAt(e, pos)` | entity | `bot.activate_entity_at(e, pos)` | diverges | `look_at(position)` before the interaction like upstream; EntityInteractEvent with explicit location |
| `bot.useOn(target)` | entity | `bot.use_on(e)` | ported | same interact packet as activateEntity, like upstream; azalea double-sends `s_interact` vs upstream's one |
| `bot.mount(e)` | entity | `bot.mount(e)` | ported | same interact packet as upstream's useEntity(target,0); azalea double-sends `s_interact` vs upstream's one |
| `bot.dismount()` | entity | `bot.dismount()` | diverges | sends shift-press player_input (the real dismount); upstream's `jump:true` literal looks like an upstream bug |
| `bot.moveVehicle(left, fwd)` | entity | `bot.move_vehicle(left:i8, fwd:i8)` | diverges | sends raw player_input like upstream, but doesn't integrate with azalea's PhysicsState |
| `bot.getExplosionDamages(e,pos,r,raw)` | entity | `bot.get_explosion_damages(..) -> Option<f32>` | diverges | exposure raycasts loaded chunks (Collider shapes); armor from tracked c_update_attributes → None when unknown, like upstream's null |
| `bot.elytraFly()` | entity | `bot.elytra_fly() -> Result<()>` | diverges | same precondition errors; upstream's elytra-equipped check is absent — the server rejects the packet without one |
| `bot._playerFromUUID(uuid)` | entity | `bot.player_by_uuid(&str) -> Option<Player>` | ported | internal upstream; exposed for parity |
| `bot.entity` | entity | `bot.entity() -> azalea::EntityRef` | ported | azalea Client::entity() |
| `bot.entities` | entity | `bot.entities() -> HashMap<MinecraftEntityId, EntityRef>` | ported | keyed by protocol entity id like upstream's `{[id]: Entity}` |
| `bot.player` | entity | `bot.player() -> Option<Player>` | ported | — |
| `bot.players` | entity | `bot.players() -> HashMap<String, Player>` | ported | tab list, keyed by username (upstream) not UUID (azalea) |
| `bot.uuidToUsername (undoc)` | entity | `bot.uuid_to_username() -> HashMap<String,String>` | ported | — |
| `bot.vehicle (undoc)` | entity | `bot.vehicle() -> Option<EntityRef>` | ported | tracked from c_set_passengers/c_set_entity_link |
| `bot.fireworkRocketDuration` | entity | `bot.firework_rocket_duration() -> u32` | diverges | random jitter term is deterministic (hash of entity id), not per-call rand |
| `Entity.skinData` | entity | `EntityExt::skin_data() -> Option<SkinData>` | ported | decoded from textures profile property |
| `Entity.vehicle` | entity | `bot.entity_vehicle(e) -> Option<EntityRef>` | ported | — |
| `Entity.position` | entity | `entity.position() -> Vec3` | ported | azalea Position component |
| `Entity.velocity` | entity | `EntityExt::velocity() -> Option<Vec3>` | ported | Physics.velocity; Option because dead entities lack the component |
| `Entity.yaw / pitch` | entity | `EntityExt::look_direction() -> Option<LookDirection>` | ported | azalea LookDirection component |
| `Entity.onGround` | entity | `EntityExt::on_ground() -> Option<bool>` | ported | — |
| `Entity.height` | entity | `entity.dimensions().height` | ported | azalea EntityDimensions |
| `Entity.eyeHeight` | entity | `entity.dimensions().eye_height` | ported | azalea EntityDimensions |
| `Player.username` | entity | `player.username() -> &str` | ported | — |
| `Player.displayName` | entity | `player.display_name() -> Option<&FormattedText>` | diverges | None when unset; upstream falls back to username |
| `Player.gamemode` | entity | `player.gamemode() -> GameMode` | ported | — |
| `Player.ping` | entity | `player.ping() -> i32` | ported | PlayerInfo.latency |
| `Player.entity` | entity | `player.entity -> Option<EntityRef>` | ported | pub field on Player struct |
| `Player.uuid` | entity | `player.info.uuid` | ported | pub field on azalea PlayerInfo |
| `Player.skinData` | entity | `player.skin_data() -> Option<SkinData>` | ported | — |
| `Player.profileKeys` | entity | `player.profile_keys -> Option<ProfileKeys>` | ported | `{public_key, signature}` bytes from `c_player_info_update`'s `initialize_chat` — azalea's `PlayerInfo` drops `chat_session`, so the bridge tracks it wire-side; cleared on re-add/`player_info_remove` |
| `event('entitySpawn')` | entity | `BotEvent::EntitySpawn(EntityRef)` | ported | c_add_entity + self on Spawn, like upstream |
| `event('entityGone')` | entity | `BotEvent::EntityGone{entity,id}` | diverges | entity is Option<EntityRef> — azalea clears the index before our packet event; id always present |
| `event('entityMoved')` | entity | `BotEvent::EntityMoved(EntityRef)` | ported | pos/rot/teleport/head-rotation packets |
| `event('entityUpdate')` | entity | `BotEvent::EntityUpdate(EntityRef)` | ported | c_set_entity_data |
| `event('entityAttributes')` | entity | `BotEvent::EntityAttributes(EntityRef)` | ported | c_update_attributes; raw snapshots tracked (azalea drops armor) |
| `event('entitySwingArm')` | entity | `BotEvent::EntitySwingArm(EntityRef)` | ported | c_animate action 0 |
| `event('entityHurt')` | entity | `BotEvent::EntityHurt{entity,source}` | ported | hurt animation + status 2 + c_damage_event; source only from damage_event |
| `event('entityDead')` | entity | `BotEvent::EntityDead(EntityRef)` | ported | entity-event status 3 |
| `event('entityTaming')` | entity | `BotEvent::EntityTaming(EntityRef)` | ported | status 6 |
| `event('entityTamed')` | entity | `BotEvent::EntityTamed(EntityRef)` | ported | status 7 |
| `event('entityShakingOffWater')` | entity | `BotEvent::EntityShakingOffWater(EntityRef)` | ported | status 8 |
| `event('entityEatingGrass')` | entity | `BotEvent::EntityEatingGrass(EntityRef)` | ported | status 10 |
| `event('entityHandSwap')` | entity | `BotEvent::EntityHandSwap(EntityRef)` | ported | status 55; tracked equipment main/off swapped like upstream |
| `event('entityWake')` | entity | `BotEvent::EntityWake(EntityRef)` | ported | animate 2 |
| `event('entityEat')` | entity | `BotEvent::EntityEat(EntityRef)` | diverges | animate 3 is the off-hand swing in this protocol; upstream's eat mapping kept for parity |
| `event('entityCriticalEffect')` | entity | `BotEvent::EntityCriticalEffect(EntityRef)` | ported | animate 4 |
| `event('entityMagicCriticalEffect')` | entity | `BotEvent::EntityMagicCriticalEffect(EntityRef)` | ported | animate 5 |
| `event('entityCrouch')` | entity | `BotEvent::EntityCrouch(EntityRef)` | ported | per packet carrying `shared_flags` with the sneak bit set (packet-content gating, like upstream) |
| `event('entityUncrouch')` | entity | `BotEvent::EntityUncrouch(EntityRef)` | ported | flag packet with the sneak bit clear while previously crouched |
| `event('entityEquip')` | entity | `BotEvent::EntityEquip(EntityRef)` | ported | c_set_equipment decoded ourselves (azalea logs-and-drops it) |
| `event('entitySleep')` | entity | `BotEvent::EntitySleep(EntityRef)` | ported | packet carrying `sleeping_pos` (idx 14, Some) or `pose` (idx 6) == Sleeping, like upstream |
| `event('entityElytraFlew')` | entity | `BotEvent::EntityElytraFlew(EntityRef)` | ported | `shared_flags` fall-fly bit false→true edge |
| `event('entityAttach')` | entity | `BotEvent::EntityAttach{entity,vehicle}` | diverges | driven by c_set_entity_link (leashes); upstream also uses attach_entity for riders — passengers tracked separately (Mounted/Dismounted) |
| `event('entityDetach')` | entity | `BotEvent::EntityDetach{entity,vehicle}` | diverges | see entityAttach |
| `event('entityEffect')` | entity | `BotEvent::EntityEffect{entity,effect,data}` | ported | c_update_mob_effect |
| `event('entityEffectEnd')` | entity | `BotEvent::EntityEffectEnd{entity,effect}` | ported | c_remove_mob_effect |
| `event('entityPlaced') (undoc)` | entity | `BotEvent::EntityPlaced(EntityRef)` | ported | emitted by `place_entity` after the spawn packet |
| `event('itemDrop')` | entity | `BotEvent::ItemDrop(EntityRef)` | ported | item-stack metadata entry, like upstream |
| `event('playerCollect')` | entity | `BotEvent::PlayerCollect{collector,collected}` | ported | c_take_item_entity |
| `event('playerJoined')` | entity | `BotEvent::PlayerJoined(PlayerInfo)` | ported | first AddPlayer per uuid (a re-add is `PlayerUpdated`, like upstream's `bot.players` membership check) |
| `event('playerUpdated')` | entity | `BotEvent::PlayerUpdated(PlayerInfo)` | ported | updates for never-added uuids are dropped |
| `event('playerLeft')` | entity | `BotEvent::PlayerLeft(PlayerInfo)` | ported | drops removes for unknown uuids and for the bot's own entry (self check runs before the membership delete, like upstream); membership cleared on the `c_login` packet like upstream's `bot.players = {}` |
| `event('usedFirework')` | entity | `BotEvent::UsedFirework(MinecraftEntityId)` | diverges | payload is the firework entity id; upstream passes the raw id too |

## Movement / physics / controls

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.setControlState(c, state)` | movement | `bot.set_control_state(Control, bool)` | ported | writes PhysicsState/Jumping components directly (Client::walk/sprint would clobber other inputs); silent no-op pre-spawn. Divergence: opposing keys collapse (fwd+back -> none, releasing one doesn't resume) since WalkDirection can't hold contradictory pairs |
| `bot.getControlState(c)` | movement | `bot.control_state(Control) -> bool` | ported | reads azalea components — reflects pathfinder movement too |
| `bot.clearControlStates()` | movement | `bot.clear_control_states()` | ported | — |
| `bot.look(yaw, pitch, force?)` | movement | `bot.look(yaw, pitch, force).await` | ported | radians, upstream convention; resolves after the tick carrying the rotation (upstream eases at yaw/pitchSpeed, azalea snaps); force skips the wait |
| `bot.lookAt(point, force?)` | movement | `bot.look_at(vec3, force).await` | ported | eye-height + upstream atan2 trig -> look() |
| `bot.waitForTicks(n)` | movement | `bot.wait_for_ticks(n) -> Result<()>` | ported | azalea wait_ticks + upstream's n*50ms+5s timeout |
| `bot._replyOnNextTick(reply)` | movement | — | wontfix | internal upstream plumbing; azalea handles teleport acks itself, no reply queue needed |
| `bot.controlState` | movement | `bot.control_states() -> ControlState` | ported | snapshot struct (not a live get/set proxy); mutate via set_control_state |
| `bot.physics` | movement | `bot.physics() -> PhysicsSettings` | diverges | read-only struct of vanilla constants; upstream's knobs are mutable, azalea physics has no mutation seam |
| `bot.physicsEnabled` | movement | `bot.physics_enabled() -> bool` | diverges | always true; azalea physics runs unconditionally |
| `bot.jumpQueued (undoc)` | movement | `bot.jump_queued() -> bool` | diverges | reads held Jumping input; upstream's flag is a queued one-shot cleared per tick |
| `bot.jumpTicks (undoc)` | movement | — | wontfix | autojump cooldown; azalea intentionally implements no autojump |
| `bot.abilities` | movement | `bot.abilities() -> PlayerAbilities` | ported | PlayerAbilities component, default before first packet |
| `event('move')` | movement | `BotEvent::Move{old_pos}` | ported | per-tick Position/LookDirection diff in bridge = upstream's per-sent-packet signal |
| `event('forcedMove')` | movement | `BotEvent::ForcedMove` | ported | on ClientboundPlayerPosition packet (teleport/spawn) |
| `event('mount')` | movement | `BotEvent::Mounted{vehicle: Option<EntityRef>}` | diverges | c_set_passengers diff in the entity tracker, edge-detected — upstream re-emits `mount` on *every* set_passengers packet that includes the bot (repeat packets produce extra events ours suppresses; ours is the sane behavior) |
| `event('dismount')` | movement | `BotEvent::Dismounted{vehicle: Option<EntityRef>}` | ported | c_set_passengers diff + vehicle-despawn path (entity tracker) |
| `event('physicsTick')` | movement | `BotEvent::Tick` | ported | azalea Event::Tick |
| `event('physicTick') (dep)` | movement | `alias of physicsTick` | ported | deprecated; same BotEvent::Tick |
| `event('abilities')` | movement | `BotEvent::Abilities(abilities)` | ported | on ClientboundPlayerAbilities packet |

## Inventory / containers / windows

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.equip(item, dest)` | inventory | `bot.equip(&item, EquipDest).await -> Result<()>` | ported | `EquipDest` enum; hand = quickbar select / first empty / rotating fallback slot like upstream |
| `bot.unequip(dest)` | inventory | `bot.unequip(dest).await` | ported | hand → empty quickbar slot/`equipEmpty`; others require closed window like upstream |
| `bot.tossStack(item)` | inventory | `bot.toss_stack(&item).await` | ported | pickup + outside click + window close, upstream order |
| `bot.toss(type, meta, count)` | inventory | `bot.toss(kind, metadata, Option<count>, nbt).await` | ported | metadata = `damage` component; `count: None` = 1 like upstream `count ?? 1` |
| `bot.setQuickBarSlot(slot)` | inventory | `bot.set_quick_bar_slot(u8)` | ported | 0..=8 validated; emits `heldItemChanged`; azalea `set_selected_hotbar_slot` + tracker override for the component-write lag |
| `bot.getEquipmentDestSlot(dest)` | inventory | `bot.equipment_dest_slot(dest) -> u16` | ported | head/torso/legs/feet = 5..8, off-hand 45, hand = 36 + quickBarSlot |
| `bot.consume()` | inventory | `bot.consume().await` | ported | `ALWAYS_CONSUMABLES` + "Food is full" guard; finishes on status 9 / matching cooldown / held type-or-count change; 2.5s `CONSUME_TIMEOUT`; re-entry cancels the earlier call |
| `bot.activateItem(offHand)` | inventory | `bot.activate_item(off_hand)` | ported | writes `s_use_item` directly (empty hand is a no-op like upstream); sets `usingHeldItem` |
| `bot.deactivateItem()` | inventory | `bot.deactivate_item()` | ported | `s_player_action` ReleaseUseItem + clears `usingHeldItem` |
| `bot.simpleClick.leftMouse(slot)` | inventory | `bot.simple_click_left(slot).await` | ported | — |
| `bot.simpleClick.rightMouse(slot)` | inventory | `bot.simple_click_right(slot).await` | ported | — |
| `bot.clickWindow(slot,btn,mode)` | inventory | `bot.click_window(slot: i32, btn, mode).await` | ported | modes 0–4 → `ClickOperation` (3 = middle/clone click); -999 outside; modes 5/6 (quick-craft/pickup-all) error; crafting-region clicks await the result resync like `waitForWindowUpdate` |
| `bot.putSelectedItemRange(..)` | inventory | `bot.put_selected_item_range(start, end, &window, reserved).await` | ported | merge not-full → first empty → reserved → toss leftover |
| `bot.putAway(slot)` | inventory | `bot.put_away(slot).await` | ported | shift-click when there's room; else pickup + place; awaits `updateSlot` for the slot |
| `bot.closeWindow(window)` | inventory | `bot.close_window(&window)` / `window.close()` | ported | `copyInventory` of the player region + `container_close`; idempotent; `windowClose` once |
| `bot.transfer(options)` | inventory | `bot.transfer(TransferOpts).await` | ported | `TransferOpts { window, kind, metadata, nbt, count, source_start/end, dest_start/end }`; `count: None` = 1 (`count ?? 1`); dest -999 drops; left-click bulk / right-click per-item + half-stack shortcut; leftovers restored via putSelectedItemRange |
| `bot.openBlock(block, dir, cursor)` | inventory | `bot.open_block(&block).await -> Result<Window>` | diverges | `look_at(block center)` + `block_interact` + `windowOpen` wait; azalea fabricates the block-face hit — no `direction`/`cursorPos` params |
| `bot.openEntity(entity, Class?)` | inventory | `bot.open_entity(&EntityRef).await -> Result<Window>` | ported | `look_at(entity)` + `entity.interact()` + `windowOpen` wait (5s timeout) |
| `bot.moveSlotItem(src, dst)` | inventory | `bot.move_slot_item(src, dst).await` | ported | clicks `src` back when the swap leaves a stack on the cursor |
| `bot.updateHeldItem()` | inventory | `bot.update_held_item()` | ported | re-emits `heldItemChanged` |
| `bot.openContainer(b\|e, ..)` | inventory | `bot.open_container(impl Into<ContainerTarget>).await` | ported | `ContainerTarget::Block|Entity`; upstream allowed-name precheck + container menu-kind check (generic_9x*/generic_3x3/hopper/shulker_box); on mismatch we close the window then error — upstream throws leaving it open |
| `bot.openChest(..) (dep)` | inventory | `bot.open_chest(..)` — alias of `open_container` | ported | identical to upstream (one shared function) |
| `bot.openFurnace(block)` | inventory | `bot.open_furnace(b).await -> Furnace` | ported | open_block + menu-kind check (furnace/blast_furnace/smoker); on mismatch we close the window then error — upstream leaves it open |
| `bot.openDispenser(..) (dep)` | inventory | `bot.open_dispenser(..)` — alias of `open_container` | ported | identical to upstream (one shared function) |
| `bot.openEnchantmentTable(b)` | inventory | `bot.open_enchantment_table(b).await` | ported | `enchanting_table` name check before opening, then `enchantment` menu-kind check |
| `bot.openAnvil(b)` | inventory | `bot.open_anvil(b).await` | ported | `anvil` menu kind covers chipped/damaged like upstream's regex |
| `bot.openVillager(e)` | inventory | `bot.open_villager(e).await` | ported | villager entity-kind check + `merchant` menu kind, waits `VillagerReady` (merchant_offers) before returning |
| `bot.trade(villager, idx, times?)` | inventory | `bot.trade(&v, idx, count)` / `villager.trade(idx,count)` | ported | s_select_trade + updateSlot:0/1/2 waits + per-iteration putAway(2); `count: None` = all remaining uses; leftover inputs put back |
| `bot.writeBook(slot, pages)` | inventory | `bot.write_book(slot, pages).await` | ported | s_edit_book; non-hotbar books move to slot 36 + wait slot echoes like upstream's _syncWindow; bounded ack wait for the slot update |
| `bot.signBook(slot,pages,author,title) (undoc)` | inventory | `bot.sign_book(..).await` | diverges | same dance; `author` is accepted but unused — the server credits the signing player (upstream's packet path doesn't send it either) |
| `bot.setCommandBlock(pos, cmd, opts)` | inventory | `bot.set_command_block(..)` | ported | creative + `command_block` name checks; azalea splits upstream's flag bitfield into track_output/conditional/automatic bools |
| `bot.inventory` | inventory | `bot.inventory() -> Window` | ported | id-0 player window; reads `inventory_menu` directly so it stays live while a container is open (azalea's handle gating would return `None`) |
| `bot.quickBarSlot` | inventory | `bot.quick_bar_slot() -> u8` | ported | — |
| `bot.heldItem` | inventory | `bot.held_item() -> Option<Item>` | ported | — |
| `bot.usingHeldItem` | inventory | `bot.using_held_item() -> bool` | ported | — |
| `bot.currentWindow (undoc)` | inventory | `bot.current_window() -> Option<Window>` | ported | shared `Arc` window; `None` when only the inventory is open |
| `bot.QUICK_BAR_START (undoc)` | inventory | `const QUICK_BAR_START: usize` | ported | = 36 |
| `Window.items()` | inventory | `window.items() -> Vec<Item>` | ported | — |
| `Window.slots` | inventory | `window.slots() -> Option<Vec<ItemStack>>` | ported | live menu clone; `None` when closed |
| `Window.deposit(type,meta,count,nbt)` | inventory | `window.deposit(..).await` | ported | player → container region via `transfer`; `count: None` = 1 |
| `Window.withdraw(type,meta,count,nbt)` | inventory | `window.withdraw(..).await` | ported | container → player region; `count: None` = 1; errors on full inventory like upstream |
| `Window.close()` | inventory | `window.close()` (sync) | ported | copyInventory + close packet + `WindowClose`; idempotent |
| `Chest / Container class` | inventory | `type Chest = Window` | ported | upstream adds no surface over Window; `deposit`/`withdraw`/`close` shared |
| `Dispenser class` | inventory | `type Dispenser = Window` | ported | same as Chest |
| `window event 'open'` | inventory | `BotEvent::WindowOpen(Window)` | ported | window-level events are bot-level here (windows aren't emitter objects) |
| `window event 'close'` | inventory | `BotEvent::WindowClose(Window)` | ported | — |
| `window event 'updateSlot'` | inventory | `BotEvent::WindowUpdateSlot{window_id,slot,old,new}` | ported | also emitted for `set_slot` container_id -2 (player-inventory applies upstream never saw — azalea applies them, so reporting is strictly truer) |
| `Furnace class` | inventory | `struct Furnace` (Deref → Window) | ported | — |
| `Furnace.fuel` | inventory | `furnace.fuel() -> f32` | ported | `fuelSeconds()`/`progressSeconds()` too; 0 until the data slots arrive like upstream's null guards |
| `Furnace.progress` | inventory | `furnace.progress() -> f32` | ported | — |
| `Furnace.totalFuel` | inventory | `furnace.total_fuel() -> Option<i32>` | ported | raw server-reported burn ticks; `total_fuel_seconds()` too; `None` until the data slot arrives like upstream's `null` |
| `Furnace.totalProgress` | inventory | `furnace.total_progress() -> Option<i32>` | ported | raw smelt ticks; `total_progress_seconds()` too |
| `Furnace.takeInput()` | inventory | `furnace.take_input().await -> Item` | ported | — |
| `Furnace.takeFuel()` | inventory | `furnace.take_fuel().await` | ported | — |
| `Furnace.takeOutput()` | inventory | `furnace.take_output().await` | ported | — |
| `Furnace.putInput(type,meta,count)` | inventory | `furnace.put_input(..).await` | ported | `count: None` = 1 like upstream's `?? 1` |
| `Furnace.putFuel(...)` | inventory | `furnace.put_fuel(..).await` | ported | — |
| `Furnace.inputItem()` | inventory | `furnace.input_item() -> Item` | ported | — |
| `Furnace.fuelItem()` | inventory | `furnace.fuel_item()` | ported | — |
| `Furnace.outputItem()` | inventory | `furnace.output_item()` | ported | — |
| `furnace event 'update'` | inventory | `BotEvent::FurnaceUpdate{window_id}` | diverges | bot-level event (windows aren't emitters); fires per container_set_data on furnace-kind windows; raw `WindowDataUpdate` also emitted |
| `EnchantmentTable class` | inventory | `struct EnchantmentTable` (Deref → Window) | ported | — |
| `EnchantmentTable.xpseed` | inventory | `enchant.xpseed()` | ported | -1 until property 3 arrives |
| `EnchantmentTable.enchantments` | inventory | `enchant.enchantments() -> [EnchantmentOption;3]` | diverges | `{level, expected}`; the clue id resolves through the server's dynamic registry to an `EnchantmentKey` (upstream exposes the raw int) |
| `EnchantmentTable.targetItem()` | inventory | `enchant.target_item()` | ported | — |
| `EnchantmentTable.enchant(choice)` | inventory | `enchant.enchant(0..2).await` | ported | s_container_button_click after the ready gate; resolves the new slot-0 item |
| `EnchantmentTable.takeTargetItem()` | inventory | `enchant.take_target_item().await` | ported | — |
| `EnchantmentTable.putTargetItem(item)` | inventory | `enchant.put_target_item(..).await` | ported | — |
| `EnchantmentTable.putLapis(item)` | inventory | `enchant.put_lapis(..).await` | ported | — |
| `enchant event 'ready'` | inventory | `BotEvent::EnchantmentTableReady{window_id}` | diverges | bot-level, edge-triggered when all 3 level costs are known; unlatches on -1 like upstream |
| `Anvil class` | inventory | `struct Anvil` (Deref → Window) | ported | `repair_cost()` exposes container_set_data prop 0 |
| `Anvil.combine(a,b,name?)` | inventory | `anvil.combine(..).await` | diverges | same click choreography + s_rename_item prefix sends, but order/xp are checked from the *server-reported* cost (azalea has no client-side `Item.anvil`); tries the swapped order when the first isn't affordable |
| `Anvil.rename(item,name) (undoc)` | inventory | `anvil.rename(..).await` | diverges | as combine — server-reported cost; creative skips the xp wait (upstream waits unconditionally) |
| `Villager class` | inventory | `struct Villager` (Deref → Window) | ported | Merchant menu |
| `Villager.trades` | inventory | `villager.trades() -> Vec<VillagerTrade>` | ported | decoded merchant_offers incl. realPrice formula |
| `Villager.selectedTrade` | inventory | `villager.selected_trade()` | ported | — |
| `Villager.trade(index,count?)` | inventory | `villager.trade(idx,count).await` | ported | — |
| `villager event 'ready'` | inventory | `BotEvent::VillagerReady{window_id}` | diverges | bot-level, once per window on first merchant_offers |
| `event('windowOpen')` | inventory | `BotEvent::WindowOpen(Window)` | ported | emitted on first `container_set_content` after `open_screen`, upstream's `setWindowItems` gate |
| `event('windowClose')` | inventory | `BotEvent::WindowClose(Window)` | diverges | client + server closes, once each; upstream also emits `windowClose(null)` on a close packet with no tracked window — the typed `Window` payload can't express null, so that event is skipped |
| `event('heldItemChanged')` | inventory | `BotEvent::HeldItemChanged(Option<Item>)` | ported | emitted wherever upstream's `updateHeldItem()` runs — set_held_slot, every resolved `set_slot`/`window_items` incl. pending windows and `lastClosedWindow` remaps, setQuickBarSlot |
| `bot._setSlot (internal)` | inventory | `windows::observe_event` | ported | folded into the bridge-side tracker (slot snapshots, old/new events, azalea-gap patches) |
| `bot._syncWindow (internal)` | inventory | `bot.sync_window(&window).await` | ported | raw `ServerboundContainerClick` carrying an impossible `state_id` (`u32::MAX` — wire-identical to upstream's `-1`), slot `-999`, QuickCraft end-drag button 2; awaits the server's corrective `SetWindowItems` (5 s cap) before returning; azalea's `ContainerHandleRef::click` can't express it (always sends the real `state_id`) |
| `bot._ensureHasSentCarriedItem (internal)` | inventory | `azalea container_click packet` | ported | click packets carry the cursor stack |
| `event confirmTransaction (internal)` | inventory | — | diverges | no `transaction` wire on mc26 — the packet was removed in 1.17 (upstream's own handler is dead code on `transactionPacketExists`-false versions); click rejects surface as corrective `container_set_slot`/`set_content` resyncs → `WindowUpdateSlot`/`SetWindowItems` |
| `event setSlot:{windowId} (internal)` | inventory | `BotEvent::WindowUpdateSlot{window_id,slot,old,new}` | ported | fires on every upstream `_setSlot` path (resolved `set_slot` incl. announced-but-unfilled windows, `set_player_inventory`→0, `lastClosedWindow` remap→0) with a superset payload (adds the slot index) |
| `event setWindowItems:{windowId} (internal)` | inventory | `BotEvent::SetWindowItems{window_id}` | ported | per resolved `container_set_content`, after the per-slot updates + `heldItemChanged`, before `windowOpen` on first fill; not emitted for early-buffered or `lastClosedWindow`-remapped content, matching upstream |

## Crafting / recipes

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.recipesFor(type,meta,min,table)` | craft | `bot.recipes_for(kind, metadata, min, table) -> Vec<Recipe>` | diverges | filter semantics ported (result/metadata/minResultCount/table + inventory-delta simulation), but the recipe source is the server's recipe book (`c_recipe_book_add/remove`), not a static data table — only unlocked recipes are listed |
| `bot.recipesAll(type,meta,table)` | craft | `bot.recipes_all(kind, metadata, table) -> Vec<Recipe>` | diverges | same book-scoped source as `recipes_for`; ignores inventory, respects the table requirement |
| `bot.craft(recipe, count?, table?)` | craft | `bot.craft(&recipe, count, table).await` | ported | same book-free click engine as upstream craft.js: pick stack → right-click one per cell → putMaterialsAway → take slot 0 → clear grid; no `s_place_recipe`; `count` None → 1; table recipes error without a table `Block`; table window via `open_block`; success path runs `bot.sync_window` before close — upstream's `_syncWindow` force-resync so unconfirmed put-away predictions can't bake into the inventory view |
| `Recipe class` | craft | `struct Recipe` | diverges | `result`/`inShape`/`outShape`/`ingredients`/`delta`/`requiresTable` ported; ingredient cells generalize upstream's `RecipeItem` to `Ingredient{any_of}` alternative sets (vanilla `#tag` ingredients), resolved via azalea's static item tags; `out_shape` not populated by the book source |

## Block interaction (dig / place / activate)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.dig(block, forceLook, digFace)` | interact | `bot.dig(block).await` / `bot.dig_opts(block, ForceLook, DigFace).await` | diverges | `start_mining` + `Mining` watch; resolves on air, `'Digging aborted'` on stop/interrupt/timeout — the timeout is additive: upstream waits for the air blockUpdate with no deadline, ours bounds the wait at dig_time + 10 s (`DIG_FINISH_GRACE`) and returns `Error::Timeout` |
| `bot.stopDigging()` | interact | `bot.stop_digging()` | ported | `StopMiningBlockEvent` → AbortDestroyBlock + `MiningQueued` drop |
| `bot.digTime(block)` | interact | `bot.dig_time(block) -> Option<Duration>` | diverges | `None` = upstream `Infinity`; azalea `get_mine_progress`, `ceil(1/p)` ticks → ms |
| `bot.canDigBlock(block)` | interact | `bot.can_dig_block(block) -> bool` | ported | diggable && eye-to-center ≤ 5.1 |
| `bot.placeBlock(ref, faceVec)` | interact | `bot.place_block(block, face).await -> Result<Block>` / `bot.place_block_opts(block, face, &PlaceOpts).await` | ported | same blockUpdate ack→dest gate, "still unchanged" refusal, and 5s bound as upstream (`onceWithCleanup(timeout: 5000)`); plain `place_block` = upstream's `{swingArm: 'right'}` default |
| `bot.placeEntity(ref, faceVec)` | interact | `bot.place_entity(..).await -> Result<EntityRef>` / `bot.place_entity_opts(.., &PlaceOpts).await` | diverges | EntitySpawn correlation within maxDistance (bat 4 / boat 3 / else 2), boat sends use_item; same 5s bound as upstream; spawn-egg places force `showHand:false` like upstream — the swing packet then carries the wire default `hand:0` (main), same as upstream's omitted field |
| `bot.activateBlock(b, dir, cursor)` | interact | `bot.activate_block(b, dir, cursor).await` | ported | raw `ServerboundUseItemOn` — cursor hits the wire (azalea `block_interact` re-raycasts instead); upstream's hand=0 + always-look + always-swing defaults |
| `_genericPlace options` | interact | `PlaceOpts { offhand, half, delta, force_look, swing_arm, show_hand }` | diverges | `offhand` requires slot 45 and sends `hand: 1`; `half` nudges the cursor 0.25 on side faces; `delta` overrides the cursor outright; `force_look` covers true/false/`'ignore'`; `swing_arm: Option<InteractionHand>` (`None` = no swing); `show_hand:false` sends the wire default `hand:0` (main) like upstream's omitted `hand` field — cosmetic only (swing is a visual packet) |
| `bot._nextSequence (internal)` | interact | `Bot::next_sequence()` (private) | ported | `BlockStatePredictionHandler::start_predicting` |
| `bot.targetDigBlock (undoc)` | interact | `bot.target_dig_block() -> Option<Block>` | ported | azalea `Mining`/`MiningQueued` pos |
| `bot.targetDigFace (undoc)` | interact | `bot.target_dig_face() -> Option<Direction>` | diverges | azalea `Direction` (same 0–5 ordinals), engine-derived from the hit result |
| `bot.lastDigTime (undoc)` | interact | `bot.last_dig_time() -> Option<Instant>` | ported | set on dig resolve or abort, like upstream |
| `bot._genericPlace (internal)` | interact | `Bot::generic_place()` (private) + `Bot::use_item_on()` | ported | look at click point (skipped on `forceLook:'ignore'`) → `ServerboundUseItemOn` with the selected hand → optional swing — upstream's packet-then-swing order |
| `bot._placeBlockWithOptions (internal)` | interact | `Bot::place_block()` body | ported | — |
| `bot._placeEntityWithOptions (internal)` | interact | `Bot::place_entity()` body | ported | — |
| `event('diggingCompleted')` | interact | `BotEvent::DiggingCompleted(newBlock)` | ported | payload is the new (usually air) block, like upstream |
| `event('diggingAborted')` | interact | `BotEvent::DiggingAborted{block, reason}` | diverges | adds a `DigAbortReason` (stopped/interrupted/timeout) |

## Scoreboard / teams / bossbar / title

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.scoreboards` | misc | `bot.scoreboards() -> HashMap<String,ScoreBoard>` | ported | cloned snapshot; reset each login |
| `bot.scoreboard` | misc | `bot.scoreboard(ScoreboardSlot) -> Option<ScoreBoard>` | diverges | slot-enum accessor instead of a slot-keyed map; bindings store objective names (upstream stores the board objects), and removing an objective clears every slot bound to it — upstream clears only the first, leaving stale bindings there |
| `bot.teams` | misc | `bot.teams() -> HashMap<String,Team>` | ported | cloned snapshot |
| `bot.teamMap` | misc | `bot.team_map() -> HashMap<String,Team>` | ported | cloned snapshot |
| `bot.bossBars (undoc)` | misc | `bot.boss_bars() -> Vec<BossBar>` | ported | — |
| `ScoreBoard.name` | misc | `scoreboard.name` (field) | ported | — |
| `ScoreBoard.title` | misc | `scoreboard.title` (field) | ported | `FormattedText`, not pre-rendered |
| `ScoreBoard.itemsMap` | misc | `scoreboard.items_map() -> &HashMap` | ported | — |
| `ScoreBoard.items` | misc | `scoreboard.items() -> Vec<&ScoreItem>` | ported | descending score like upstream |
| `ScoreItem fields` | misc | `item.{name,value,display}` | diverges | `value` is `i32` — the wire is a signed VarInt (azalea decodes `u32`; un-wrapped at the boundary so negative scores don't wrap to ~4.29e9); the `displayName` getter is `item.display_name(&bot.team_map())` — team-styled via `Team::member_display_name`, plain name off a team |
| `ScoreBoard.setTitle(t)` | misc | `scoreboard.set_title(FormattedText)` | ported | client-side only, like upstream |
| `ScoreBoard.add(name,value)` | misc | `scoreboard.add(name, value: i32)` | ported | signed score, matching the wire |
| `ScoreBoard.remove(name)` | misc | `scoreboard.remove(&str)` | ported | — |
| `Team fields` | misc | `team.{name,display_name,friendly_fire,see_friendly_invisibles,nametag_visibility,collision_rule,color,prefix,suffix,members}` | ported | color is `ChatFormatting` (Reset = none) |
| `Team.memberMap` | misc | `n/a` | wontfix | upstream `membersMap` (typed `memberMap` in index.d.ts) maps name→`''` — internal bookkeeping only; the key set is ported as `team.members` |
| `Team.parseMessage` | misc | `n/a` | diverges | upstream internal helper; packet reducer decodes directly |
| `Team.add / remove / update` | misc | `n/a` | diverges | upstream internal helpers; state mutates via packets only |
| `Team.displayName(member)` | misc | `team.member_display_name(member) -> FormattedText` | ported | prefix + colored member + suffix |
| `BossBar fields` | misc | `bossbar.{entity_uuid,title,health,dividers,color,darken_sky,dragon_bar,create_fog}` | ported | `entity_uuid` is the bar uuid string; `flags` folded into the three bools |
| `event('scoreboardCreated')` | misc | `BotEvent::ScoreboardCreated(sb)` | ported | — |
| `event('scoreboardDeleted')` | misc | `BotEvent::ScoreboardDeleted(sb)` | diverges | skipped for objectives the client never saw — upstream emits unconditionally (payload may be `undefined`); same tradeoff as `teamRemoved` |
| `event('scoreboardTitleChanged')` | misc | `BotEvent::ScoreboardTitleChanged(sb)` | ported | — |
| `event('scoreUpdated')` | misc | `BotEvent::ScoreUpdated{scoreboard,item}` | ported | — |
| `event('scoreRemoved')` | misc | `BotEvent::ScoreRemoved{scoreboard,item}` | diverges | no scan-all-boards fallback when the named objective is absent, and nothing emitted when the owner is missing (upstream emits `undefined` item) |
| `event('scoreboardPosition')` | misc | `BotEvent::ScoreboardPosition{slot,scoreboard,previous_board}` | ported | carries the replaced objective like upstream's `previousBoard` arg (`None` for empty/dangling slots); like upstream, unknown objectives neither emit nor bind the slot |
| `event('teamCreated')` | misc | `BotEvent::TeamCreated(team)` | ported | — |
| `event('teamRemoved')` | misc | `BotEvent::TeamRemoved(team)` | diverges | carries the removed team; upstream emits `undefined` |
| `event('teamUpdated')` | misc | `BotEvent::TeamUpdated(team)` | ported | — |
| `event('teamMemberAdded')` | misc | `BotEvent::TeamMemberAdded(team)` | ported | team payload, like upstream |
| `event('teamMemberRemoved')` | misc | `BotEvent::TeamMemberRemoved(team)` | ported | team payload, like upstream |
| `event('bossBarCreated')` | misc | `BotEvent::BossBarCreated(bar)` | ported | — |
| `event('bossBarDeleted')` | misc | `BotEvent::BossBarDeleted(bar)` | ported | carries the removed bar — upstream emits before deleting, same object |
| `event('bossBarUpdated')` | misc | `BotEvent::BossBarUpdated(bar)` | ported | — |
| `event('title')` | misc | `BotEvent::Title{text,kind}` | diverges | `text` renders the whole component tree; upstream `parseTitle` returns only the root `.text` |
| `event('title_times')` | misc | `BotEvent::TitleTimes{fade_in,stay,fade_out}` | ported | — |
| `event('title_clear')` | misc | `BotEvent::TitleClear` | ported | — |

## Particles / sound

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `Particle fields` | misc | `particle.{id,name,position,offset,long_distance_render,count,movement_speed,data}` | diverges | `id` is the u32 registry id (upstream's `particle.id` is the type string); `name` is `minecraft:*` vs upstream's mcdata name |
| `Particle.fromNetwork(packet)` | misc | `Particle::from_packet(&packet)` | ported | — |
| `event('particle')` | misc | `BotEvent::ParticleSpawned(Particle)` | ported | — |
| `event('soundEffectHeard')` | misc | `BotEvent::SoundHeard{name,pos,volume,pitch}` | ported | entity sounds resolve to entity position |
| `event('hardcodedSoundEffectHeard')` | misc | `BotEvent::HardcodedSound{id,category,pos,volume,pitch}` | diverges | never emitted — azalea's registry Holder always resolves a name |
| `event('noteHeard')` | misc | `BotEvent::NoteHeard{block,instrument,pitch}` | diverges | decoded from packet action id/param; upstream re-reads the block's metadata and skips when the block is unloaded — slight over-emission |

## Time / weather / game state

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.time.doDaylightCycle` | state | `bot.time()?.do_daylight_cycle` | ported | `bot.time() -> Option<Time>`; None until first c_set_time |
| `bot.time.bigTime` | state | `bot.time()?.big_time` | ported | u64 — no BigInt/Number split needed |
| `bot.time.time` | state | `bot.time()?.time` | ported | — |
| `bot.time.timeOfDay` | state | `bot.time()?.time_of_day` | ported | — |
| `bot.time.day` | state | `bot.time()?.day` | ported | — |
| `bot.time.isDay` | state | `bot.time()?.is_day` | ported | — |
| `bot.time.moonPhase` | state | `bot.time()?.moon_phase` | ported | — |
| `bot.time.bigAge` | state | `bot.time()?.big_age` | ported | — |
| `bot.time.age` | state | `bot.time()?.age` | ported | — |
| `bot.isRaining` | state | `bot.is_raining() -> bool` | ported | c_game_event decoded (weather is a game event, not level event) |
| `bot.rainState` | state | `bot.rain_state() -> f32` | ported | — |
| `bot.thunderState` | state | `bot.thunder_state() -> f32` | ported | — |
| `bot.game.levelType` | state | `bot.game().level_type` | ported | "flat"/"default" from is_flat on login+respawn |
| `bot.game.dimension` | state | `bot.game().dimension` / `bot.dimension() -> Option<WorldName>` | ported | `bot.game()` facade exists (world.rs); `Game.dimension` populated via `bot.dimension()` |
| `bot.game.difficulty` | state | `bot.game().difficulty` | ported | c_change_difficulty decoded |
| `bot.game.gameMode` | state | `bot.game().game_mode` / `bot.game_mode() -> Option<GameMode>` | ported | `bot.game()` facade exists (world.rs); `Game.game_mode` populated via `bot.game_mode()` |
| `bot.game.hardcore` | state | `bot.game().hardcore` | ported | from c_login |
| `bot.game.maxPlayers` | state | `bot.game().max_players` | ported | from c_login |
| `bot.game.serverBrand` | state | `bot.game().server_brand` | diverges | vanilla sends minecraft:brand in *configuration* phase — unreachable via Event::Packet (game packets only); stays None |
| `bot.game.minY` | state | `bot.game().min_y` | ported | world.chunks.min_y() |
| `bot.game.height` | state | `bot.game().height` | ported | world.chunks.height() |
| `event('time')` | state | `BotEvent::Time` | ported | per c_set_time; clocks also tick forward one tick per Tick like vanilla |
| `event('rain')` | state | `BotEvent::Rain` | ported | emitted when is_raining flips |
| `event('weatherUpdate')` | state | `BotEvent::WeatherUpdate` | ported | rain/thunder level changes |
| `event('game')` | state | `BotEvent::GameStateChanged` | ported | login, respawn, gamemode change, spawn-position update |

## Health / food / experience / oxygen

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.health` | state | `bot.health() -> f32` | ported | Health component; 0.0 when gone |
| `bot.food` | state | `bot.food() -> u32` | ported | Hunger component |
| `bot.foodSaturation` | state | `bot.food_saturation() -> f32` | ported | Hunger component |
| `bot.oxygenLevel` | state | `bot.oxygen_level() -> u8` | ported | air ticks (0..=300) -> round(/15) -> 0..=20, like upstream |
| `bot.isAlive (undoc)` | state | `bot.is_alive() -> bool` | ported | azalea is_alive |
| `bot.experience.level` | state | `bot.experience() -> Experience { level, .. }` | ported | azalea Experience component |
| `bot.experience.points` | state | `bot.experience() -> Experience { total, .. }` | ported | named `total` in azalea |
| `bot.experience.progress` | state | `bot.experience() -> Experience { progress, .. }` | ported | azalea Experience component |
| `event('health')` | state | `BotEvent::Health` | ported | emitted per `set_health` packet |
| `event('death')` | state | `BotEvent::Death` | ported | once per life like upstream's `isAlive` edge — azalea raises Event::Death twice per vanilla death (player_combat_kill + health==0); deduped, set again by `c_respawn`, re-armed by `c_set_health` health > 0 (payload packet dropped) |
| `event('breath')` | state | `BotEvent::Breath` | ported | AirSupply tick diff |
| `event('experience')` | state | `BotEvent::Experience` | ported | emitted per `set_experience` packet |
| `event('spawnReset')` | state | `BotEvent::SpawnReset` | ported | `game_event` NoRespawnBlockAvailable |

## Bed / sleep

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.sleep(bedBlock)` | misc | `bot.sleep(BlockPos).await` | diverges | same flow minus upstream's client-side night/monster/occupancy checks (server-enforced) |
| `bot.wake()` | misc | `bot.wake()` | diverges | sync send of `s_player_command` StopSleeping (upstream's await only wraps packet write) |
| `bot.isABed(bedBlock)` | misc | `bot.is_a_bed(BlockState) -> bool` | diverges | takes a block *state* like upstream's block.type check; accepts any `*_bed` name — broader than upstream's 16-color whitelist + legacy `'bed'` (identical for vanilla, looser for modded/prefixed names) |
| `bot.isSleeping` | misc | `bot.is_sleeping() -> bool` | ported | `Pose::Sleeping` |
| `bot.parseBedMetadata (undoc)` | misc | `n/a` | diverges | upstream internal helper; azalea's `Pose` covers it |
| `event('sleep')` | misc | `BotEvent::Sleep` | ported | Pose diff per tick |
| `event('wake')` | misc | `BotEvent::Wake` | ported | Pose diff per tick |

## Fishing

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.fish()` | misc | `bot.fish().await` | ported | bobber spawn + 6-splash-particle radius like upstream; 2nd call cancels 1st |

## Settings

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.setSettings(options)` | misc | `bot.set_settings(Settings)` | ported | merge via `Settings { field, ..bot.settings() }`; rejects `view_distance` 0; `ViewDistance` presets cover upstream's string aliases |
| `bot.settings.chat` | misc | `bot.settings().chat_visibility` | ported | `ChatVisibility` enum |
| `bot.settings.colorsEnabled` | misc | `bot.settings().chat_colors` | ported | — |
| `bot.settings.viewDistance` | misc | `bot.settings().view_distance: ViewDistance` | diverges | `Far`/`Normal`/`Short`/`Tiny` presets + `Chunks(u8)` cover upstream's `number\|'far'\|'normal'\|'short'\|'tiny'` type (string parse via `FromStr`); read-back reports `Chunks` (upstream keeps the string you set); azalea default is 8, upstream default `'far'` is 12 |
| `bot.settings.difficulty` | misc | `bot.settings().difficulty` | ported | client-side store, default 2 |
| `bot.settings.skinParts` | misc | `bot.settings().model_customization` | ported | `ModelCustomization` flag struct |
| `bot.settings.enableTextFiltering` | misc | `bot.settings().text_filtering_enabled` | ported | — |
| `bot.settings.enableServerListing` | misc | `bot.settings().allows_listing` | ported | azalea default is `false` vs upstream `true` |
| `bot.settings.particleStatus` | misc | `bot.settings().particle_status` | ported | `ParticleStatus` enum |
| `bot.settings.mainHand (typed-only)` | misc | `bot.settings().main_hand` | ported | `HumanoidArm` |

## Resource packs

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.acceptResourcePack()` | misc | `bot.accept_resource_pack()` | diverges | only game-phase pushes are tracked (config-phase pushes — server.properties/proxy packs — never emit the event or populate state); azalea also auto-accepts every push |
| `bot.denyResourcePack()` | misc | `bot.deny_resource_pack()` | diverges | only game-phase pushes are tracked; azalea's built-in `AcceptResourcePacksPlugin` auto-accepts on arrival, so a later denial usually has no effect |
| `event('resourcePack')` | misc | `BotEvent::ResourcePack{uuid,url,hash,required}` | diverges | game-phase `resource_pack_push` only — config-phase pushes (server.properties/proxy packs, the dominant modern path) never reach `Event::Packet` |

## Creative mode (bot.creative)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.creative.setInventorySlot(slot,item)` | inventory | `bot.creative().set_inventory_slot(slot,item).await` | diverges | waits for the `container_set_slot` echo but errors `Server rejected` on the first non-matching echo — upstream ignores interleaved non-matching updates and keeps waiting the full 5 s for the requested item (and on ack-less versions skips the echo for a stats round-trip) |
| `bot.creative.clearSlot(slot)` | inventory | `bot.creative().clear_slot(slot).await` | ported | — |
| `bot.creative.clearInventory()` | inventory | `bot.creative().clear_inventory().await` | diverges | clears serially and aborts on the first error; upstream `Promise.all`s every slot clear — concurrent, and sibling clears still run when one fails |
| `bot.creative.flyTo(dest)` | inventory | `bot.creative().fly_to(Vec3).await` | ported | straight-line 0.5-block steps per 50ms |
| `bot.creative.startFlying()` | inventory | `bot.creative().start_flying()` | diverges | sets `PlayerAbilities.flying` and sends `s_player_abilities` (what the vanilla client does on double-jump); upstream only zeroes `bot.physics.gravity` locally — no packet |
| `bot.creative.stopFlying()` | inventory | `bot.creative().stop_flying()` | diverges | same packet-vs-local difference as `start_flying` |

## Plugin ecosystem (in port scope — U7/U12)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.pathfinder` | pathfinder | `bot.pathfinder() -> Pathfinder<'_>` | ported | namespace handle over `PathfinderClientExt` |
| `pathfinder.goto(goal)` | pathfinder | `pf.goto(Goal) -> Result<()>` | ported | resolves on `goal_reached`; rejects with `Error::Path(PathError::*)` carrying upstream's `err.name` — `NoPath`/`Timeout` on a failed search (facade issues `retry_on_no_path=false`; `think_timeout` bounds the search *cumulatively* across repaths like upstream's sliced-A* budget, so an unreachable-but-approachable goal rejects ~5 s instead of repathing forever — and like upstream's `monitorMovement`, the reject is promise-level: `stateGoal` keeps repathing/walking afterward), `GoalChanged` when a `set_goal` replaces the goal (a structurally identical re-issue doesn't count, mirroring `newGoal !== goal` — the value-type analog can't tell a fresh-but-equal object from the same one, so it errs toward not rejecting, N-5), `PathStopped` on `path_stop`; `Error::NotConnected` on disconnect; an already-satisfied goal resolves immediately |
| `pathfinder.setGoal(goal, dynamic?)` | pathfinder | `pf.set_goal(Goal, dynamic: bool) -> Result<()>` | ported | emits `goal_updated` then `path_reset{goal_updated}` when executing; `dynamic` re-paths via a tick watcher (see diverges row below) |
| `pathfinder.stop()` | pathfinder | `pf.stop()` | ported | `force_stop_pathfinding` + clears the goal; emits `path_stop` only when something was active, like upstream |
| `pathfinder.isMoving()` | pathfinder | `pf.is_moving() -> bool` | ported | `ExecutingPath` present — false while a path is still being calculated, matching upstream's `path.length > 0` |
| `pathfinder.isThinking()` | pathfinder | `pf.is_thinking() -> bool` | ported | `Pathfinder.is_calculating` |
| `pathfinder.haveGoal()` | pathfinder | `pf.have_goal() -> bool` | ported | facade goal set and not yet reached/stopped |
| `bot.pathfinder.goal` | pathfinder | `pf.goal() -> Option<Goal>` | ported | upstream's `stateGoal`; `None` once reached or stopped |
| `pathfinder.movements` / `setMovements(m)` | pathfinder | `pf.movements()` / `pf.set_movements(Movements)` | ported | `set_movements` emits `path_reset{movements_updated}` and re-paths, like upstream |
| `Movements class` | pathfinder | `struct Movements` | diverges | honored: `can_dig`→`allow_mining`, `allow_parkour`→edge set, `blocks_to_avoid`→+100 edge cost, `blocks_cant_break`→edge drop. Unhonored: `canOpenDoors`, `allowSprinting`, `allowFreeMotion`, `dontCreateFlow`, `dontMineUnderFallingBlock`, `maxDropDown`, all `exclusion*`/scaffolding/placement fields (azalea never opens doors or places blocks while pathing); `blocks_cant_break` also can't see breaks inside multi-block moves |
| `GoalNear` | pathfinder | `Goal::Near{pos, radius}` | diverges | `RadiusGoal` — distance is measured to block *centers*; upstream compares raw node coords (half-block offset on the boundary) |
| `GoalBlock` | pathfinder | `Goal::Block(BlockPos)` | ported | `BlockPosGoal` |
| `GoalGetToBlock` | pathfinder | `Goal::GetToBlock(BlockPos)` | ported | custom `GetToBlockGoal` — upstream's strict-adjacency `isEnd` (`\|dx\| + \|dz\| + \|dy<0?dy+1:dy\| == 1`), octagonal `distanceXZ` heuristic scaled by `COST_HEURISTIC`; azalea's `ReachBlockPosGoal` was the wrong goal entirely (it answers "can a face be seen within reach" — `GoalLookAtBlock` semantics, mapped separately below) |
| `GoalPlaceBlock` | pathfinder | — | wontfix | azalea's pathfinder never places blocks; no standable-position goal exists to wrap |
| `GoalXZ` | pathfinder | `Goal::XZ{x, z}` | ported | `XZGoal` |
| `GoalNearXZ` | pathfinder | `Goal::NearXZ{x, z, range}` | ported | custom `NearXZGoal` (azalea has none) — upstream's `distanceXZ` octagonal heuristic scaled by azalea's `COST_HEURISTIC`, `dx²+dz² <= range²` isEnd |
| `GoalY` | pathfinder | `Goal::Y{y}` | ported | `YGoal` |
| `GoalLookAtBlock` | pathfinder | `Goal::LookAtBlock(BlockPos)` | diverges | `ReachBlockPosGoal` at the default 4.5 reach — azalea raycasts to the block *center* instead of upstream's visible-faces enumeration; `options.reach`/`entityHeight` aren't plumbed |
| `GoalBreakBlock` | pathfinder | `Goal::BreakBlock(BlockPos)` | diverges | upstream's is literally `GoalLookAtBlock` — same `ReachBlockPosGoal` mapping and caveats; positions the bot, doesn't dig (upstream expects `bot.dig` after, same here) |
| `GoalInvert` | pathfinder | `Goal::Invert(Box<Goal>)` | diverges | custom goal: negated success + negated heuristic — same semantics (and same A* pathology) as azalea's deprecated `InverseGoal` |
| `GoalFollow` | pathfinder | `Goal::Follow{entity, range}` | diverges | watcher task re-issues `RadiusGoal` when the entity's block position changes and reports `path_stop` on despawn; no continuous moving goal like upstream. `dynamic` is still the `setGoal` flag — upstream's GoalFollow isn't implicitly dynamic, so `dynamic=false` ends at the first in-range tick. Inside `Goal::Any`/`All`/`Invert` a `Follow` resolves to a `RadiusGoal` snapshot of the entity's *current* position at conversion (no live tracking inside composites — upstream's monitor polls the entity each tick) |
| `GoalCompositeAny` | pathfinder | `Goal::Any(Vec<Goal>)` | ported | boxed `OrGoals` — any-success, min heuristic; nested `Follow` resolves to a position snapshot (see `GoalFollow`) |
| `GoalCompositeAll` | pathfinder | `Goal::All(Vec<Goal>)` | ported | boxed `AndGoals` — all-success, max heuristic; nested `Follow` resolves to a position snapshot (see `GoalFollow`) |
| `setGoal(g, dynamic=true)` | pathfinder | `pf.set_goal(goal, true)` | diverges | watcher re-issues the goal each tick it stops holding instead of upstream's always-re-evaluating goal; `goal_reached` is still suppressed while the goal holds; a *dead* search (azalea gave up: `retry_on_no_path=false` drop, observed or wedged) clears the goal — upstream keeps `stateGoal` and re-searches; a `thinkTimeout` expiry does NOT clear it (upstream's `stateGoal` survives `timeout` and keeps repathing) |
| `pathfinder.thinkTimeout` | pathfinder | `pf.think_timeout()` / `pf.set_think_timeout(Duration)` | ported | default 5000 ms like upstream; applied as azalea's `PathfinderOpts::max_timeout` at issue time (watchers re-read it per re-issue, so `set_think_timeout` reaches them), and enforced *cumulatively* per goal issue — searches totaling the budget without a complete path report `Timeout` (latched once per batch); the report is promise-level: a pending `goto` rejects while `stateGoal`/watchers/partial paths keep running, matching upstream's `monitorMovement`; also the `NoPath`-vs-`Timeout` classifier |
| `pathfinder.tickTimeout` / `searchRadius` | pathfinder | — | wontfix | upstream's per-tick slice budget / node radius cap have no azalea knob — azalea bounds a search by `min_timeout`/`max_timeout` only |
| `pathfinder.enablePathShortcut` / `LOSWhenPlacingBlocks` / `freeLook` | pathfinder | — | wontfix | upstream's straight-line path shortcut, place-time line-of-sight toggle, and free-look movement flag assume a path executor with block placing / free camera control; azalea never places blocks while pathing and has no `freeLook` knob |
| `pathfinder.isMining()` | pathfinder | `pf.is_mining() -> bool` | diverges | azalea's `Mining` component — true while any client-side mining runs; upstream's flag is the *pathfinder's own* `digging` state, so a manual `bot.dig` in flight over-reports |
| `pathfinder.isBuilding()` | pathfinder | `pf.is_building() -> bool` | diverges | always `false` — azalea's successors never produce scaffolding placements, so there's no building state to observe (upstream can return true) |
| `pathfinder.bestHarvestTool(block)` | pathfinder | `pf.best_harvest_tool(&Block) -> Option<Item>` | ported | argmax of azalea's `get_mine_progress` over `player_slots_range()` — the main-inventory+hotbar range upstream's `inventory.items()` covers (crafting/armor/offhand excluded); `None` on an empty inventory or when every tool's dig time is infinite (unbreakable — `digTime: Infinity` → `null` upstream) |
| `pathfinder.getPathTo(movements, goal, timeout)` | pathfinder | `pf.get_path_to(&Movements, &Goal, Option<Duration>) -> Result<PathResult>` | diverges | one synchronous `calculate_path` pass from the bot position → `PathResult{status, path}`; upstream returns the generator's first `{result}` and keeps the `astarContext` for continued computes — no incremental yields here |
| `pathfinder.getPathFromTo(movements, startPos, goal, options)` | pathfinder | `pf.get_path_from_to(&Movements, &Goal…)` | diverges | same single-pass search from an arbitrary start; `timeout` honored (`options.timeout ?? thinkTimeout`), `optimizePath`/`resetEntityIntersects`/`searchRadius`/`tickTimeout`/`startMove` have no azalea analog and aren't exposed; the preview uses a fresh `CustomPathfinderState` so it can't disturb a live goto — but it runs on the caller thread for up to `timeout`, so calling it on the bot's LocalSet thread stalls the ECS (prefer `spawn_blocking` for big searches) |
| `pathfinder.goto` error names | pathfinder | `Error::Path(PathError)` | ported | `PathError::{NoPath, Timeout, GoalChanged, PathStopped}` carry upstream's `err.name` + message text |
| `event('goal_reached')` | pathfinder | `BotEvent::GoalReached{goal}` | ported | emitted when azalea clears the goal while its success check holds; suppressed for `dynamic` goals like upstream; `goal` payload matches upstream's `(goal)` |
| `event('path_update')` | pathfinder | `BotEvent::PathUpdate{path, is_partial, status}` | ported | per-tick diff of `Pathfinder`/`ExecutingPath` (azalea's `PathFoundEvent` never reaches `Event`); fires when a path installs, a repath result merges mid-run (path growth — consumption only shrinks), or a calculation finishes while a facade goal owned it — an aborted search (`stop()`/replacement mid-calc) emits nothing, matching upstream's nulled `astarContext`. `status` mirrors upstream's `results.status` — `Success`/`Partial`/`NoPath`/`Timeout`; failures are detected two ways since azalea's goto chain runs per-Update (~17 ms) inside the 50 ms tick gap: the observed drop edge, and a steady-state wedge (facade goal set, ECS idle, goal unmet) that persists through install/calc flicker for searches that install+drop between snapshots. `Timeout` also fires — once per goal batch, latched — when searches total `think_timeout` without completing; it does not clear the goal. Hidden searches (install+drop inside the gap) aren't metered toward the budget — the wedge bounds them |
| `event('goal_updated')` | pathfinder | `BotEvent::GoalUpdated{goal, dynamic}` | ported | emitted synchronously by `set_goal`, payload like upstream's `(goal, dynamic)` |
| `event('path_stop')` | pathfinder | `BotEvent::PathStop{goal}` | ported | `stop()`, a follow goal going invalid, or azalea clearing the goal without reaching (world change, internal stop); `goal` carries the abandoned facade goal (`Some`) — upstream emits no payload |
| `event('path_reset')` | pathfinder | `BotEvent::PathReset{reason}` | diverges | only `goal_updated`/`movements_updated` reasons are observable; azalea's internal repaths (stuck, block change, chunk load, dig error, …) surface as `path_update` and can't be told apart |
| `collectBlock.collect(block)` | pathfinder | `bot.collect_block().collect(target)` / `.collect_opts(target, CollectOpts)` | diverges | `CollectTarget` accepts Block/BlockPos/BlockKind/kind-list/predicate + `.count()/.max_distance()/.point()` — upstream's item-drop-entity targets are out of scope (blocks only; the drop vacuum covers pickups); find→`Goal::LookAtBlock`→equip→dig loop re-searches per iteration (upstream's `mineBlock` paths with `GoalLookAtBlock` too) + bounded drop vacuum (additive); equips the best harvest tool before each dig like upstream's `mineBlock` (`requireHarvest` + `getFromChest`) and skips candidates that stay unharvestable; partial hauls resolve `Ok` — compare `collected.blocks.len()` to the asked count; `append` runs calls side-by-side instead of merging into one target list (a cancel still ends them together, like upstream's shared task) |
| `collectBlock.targets` | pathfinder | `n/a` | diverges | upstream's live `Targets` queue (shared across `collect` calls, fed by item-drop entities via `itemDrop`/`entityGone`); ours is per-call — `CollectTarget` matchers re-search the world each iteration instead, and drop entities aren't collectable at all |
| `collectBlock.emptyInventoryIfFull(chests, filter)` | pathfinder | `.empty_inventory_if_full(&[BlockPos], &ItemFilter)` | ported | same contract: no-op while a slot is free, else deposit filter-passing stacks nearest-chest-first until one takes them or `NoChests` errors; `collect` calls it internally with the configured (or per-call) chest list/filter |
| `collectBlock` options (`append`/`ignoreNoPath`/`chestLocations`/`itemFilter`) + fields (`chestLocations`/`itemFilter`/`movements`) | pathfinder | `CollectOpts{append, ignore_no_path, chest_locations, item_filter}` + `chest_locations()`/`set_chest_locations`/`add_chest_location`, `item_filter()`/`set_item_filter`, `movements()`/`set_movements` | diverges | `ignore_no_path` skips an unreachable candidate instead of rejecting the run — upstream declares the option but never implements it (`// TODO: options.ignoreNoPath`, CollectBlock.ts:27), so its real behavior is the default reject; `chest_locations`/`item_filter` drive `emptyInventoryIfFull`/`placeItems` (per-call override or plugin field); `movements` is pushed to the pathfinder on each collect with upstream's `dontMineUnderFallingBlock`/`dontCreateFlow` flips |
| `collectBlock.cancelTask()` | pathfinder | `bot.collect_block().cancel_task()` | ported | retires in-flight collects — they resolve `Ok` with partial hauls like upstream's `PathStopped`-swallowing catch — then waits out `CollectBlockFinished` (upstream's `once('collectBlock_finished')`); a no-op when idle |
| `collectBlock.findFromVein(block, maxBlocks, maxDistance, floodRadius)` | pathfinder | `.find_from_vein(&block)` / `.find_from_vein_opts(…)` | ported | upstream `BlockVeins.js` flood fill — same-kind neighbors within `flood_radius`, capped at `max_blocks`/`max_distance` (manhattan); defaults 100/16/1 |
| `event('collectBlock_finished')` | pathfinder | `BotEvent::CollectBlockFinished` | ported | emitted from collect's completion path on success, error, and cancel alike — upstream's `finally`; `CollectDone` stays an additive success-only report |
| `pvp.attack(target) / .stop()` | pathfinder | `bot.pvp().attack(&entity)` / `.stop()` / `.target()` | diverges | dynamic `Goal::Follow` + per-tick driver: looks at `target.position + EntityDimensions.height` (upstream's `offset(0, height, 0)`, 0.9 fallback) and swings in `attack_range` on the `melee_attack_rate` schedule; retarget on a second `attack()`; ends on stop/target death/despawn/past `view_distance`; azalea's stop is immediate — upstream's ≤5s `path_stop` settle wait has no analog; a retarget keeps one run alive so the old target never emits `stoppedAttacking` (upstream's does, late, from its un-awaited `stop()`) |
| `pvp.forceStop()` | pathfinder | `.force_stop()` | ported | clears the pathfinder goal unconditionally like upstream's `setGoal(null)` — `stop()` only drops a follow goal still pointing at its target — and emits `stoppedAttacking`; no-op when idle |
| `pvp` fields (`followRange`/`viewDistance`/`attackRange`/`meleeAttackRate`/`movements`) | pathfinder | `follow_range`/`view_distance`/`attack_range`/`melee_attack_rate`/`movements` getters+setters; `AttackRate::{RandomTicks, CooldownOffset}` | ported | upstream defaults (2 / 128 / 3.0 / `MaxDamageOffset(-2,2)` / a fresh `Movements`); the driver reads the tunables live each tick so mid-fight retunes apply — `follow_range`/`movements` still bind at `attack` like upstream's `GoalFollow`/`setMovements` |
| `event('stoppedAttacking')` | pathfinder | `BotEvent::StoppedAttacking` | diverges | emitted exactly once per attack run — by `stop()`/`force_stop()` or by the driver on death/despawn/defeat/disconnect; never on retarget (upstream's retarget emits it late, from the un-awaited `stop()`) |
| `event('startedAttacking')` | pathfinder | `BotEvent::StartedAttacking` | ported | per successful `attack()` call, retargets included |
| `event('attackedTarget')` | pathfinder | `BotEvent::AttackedTarget` | ported | per swing the driver lands, no payload like upstream |
| `tool.equipForBlock(block)` + `.bestToolForBlock` analog + `.equipForEntity(entity)` | pathfinder | `bot.tool().equip_for_block(&block)` / `.equip_for_block_opts(&block, &EquipOpts)` / `.best_tool_for(&block)` / `.equip_for_entity(&entity)` | diverges | scans the full 36-slot player inventory plus a bare-hand candidate (upstream `items()` + `unshift(undefined)`) scored by `get_mine_progress`, holding a joint-best tool as a no-op like `isBetterMiningTool`; `best_tool_for` returns `None` when the bare hand wins; `equip_for_entity` is additive — a generic max-`attack_damage` hotbar pick (no `equipForEntity` exists upstream) |
| `tool.checkWhetherToBringMoreTools` | pathfinder | `n/a` | wontfix | no API by this name exists in current upstream `mineflayer-tool` (verified `Tool.ts`/`Inventory.ts`/`TaskQueue.ts` on master and npm 1.0.2) or `mineflayer-collectblock` 1.6.0 — audited name only; the chest-retrieval role it implies is covered by `equip_for_block_opts{get_from_chest}` |
| `tool` `MiningEquipOptions`/`chestLocations`/`standardToolFilter` | pathfinder | `EquipOpts{require_harvest, get_from_chest, max_tools}` + `chest_locations()`/`set_chest_locations`/`add_chest_location` | ported | `require_harvest` filters to drop-producing candidates (azalea's private harvest check replicated as the `minecraft:tool` component-rule lookup — the mc1.20.5+ form of mcdata `harvestTools`) and errors `NoItem` like upstream; `get_from_chest`+`max_tools` visit `chest_locations` nearest-first, withdraw the name-matched tools cheapest-dig-first, then retry — our per-visit `goto` bounds at 30s where upstream waits unbounded |
| `autoEat.*` | pathfinder | `bot.auto_eat()` | ported | `AutoEat` facade over the U12 driver pattern: `enable_auto()`/`disable_auto()` toggle a per-tick monitor (`bot.food() < min_hunger || bot.health() < min_health` → one `eat()`), `eat(opts)`/`cancel_eat()`/`is_eating()`/`enabled()`/`opts()`/`set_opts()` mirror `EatUtil`; food data comes from azalea's `Food` default component (`AutoEat::food_data` covers `foodsByName`); upstream's per-plugin event emitter becomes `BotEvent::EatStart`/`EatFinish`/`EatFail` — note the brief's "`eatSuccess`" is upstream `eatFinish` (fires after `eatFail` too, from `finally`). Upstream's `eat` leaks `_eating` on an equip-failure throw; ours releases `is_eating` on every path |
| `autoEat.opts` fields | pathfinder | `AutoEatOpts` | ported | all of `IEatUtilOpts`: `priority` (`foodPoints`/`saturation`/`effectiveQuality`/`saturationRatio` → `FoodPriority`), `min_hunger` 15, `min_health` 14, `banned_food` (upstream's 5 defaults), `return_to_last_item`, `offhand`, `eating_timeout` 3 s, `strict_errors` |
| `autoEat.eat(opts)` | pathfinder | `bot.auto_eat().eat(EatOpts)` | diverges | `EatOpts` = `{food: Option<FoodSelection(Item|ItemKind|name)>, offhand, equip_old_item, priority}`; auto-pick and explicit `ItemKind`/name selection scan the player region plus offhand slot 45 (upstream `getAllItems` — armor can't hold food anyway); mid-eat failure messages match upstream's: `Item switched early to: <name>!` on a held-stack type change (an emptied slot resolves), `Eating timed out with a time of <ms> milliseconds!`, `Eating cancelled due to calling bot.consume() again` — but the held-stack check runs once after the consume wait resolves, not via a live `updateSlot` listener like upstream's `itemListener`, so a slot swapped-then-restored mid-eat resolves `Ok` where upstream rejects; explicit selections bypass `banned_food` like upstream's `normalizeFoodChoice`; numeric-id selection has no azalea analog — `ItemKind` covers it |
| `autoEat.findBestChoices(items, priority)` | pathfinder | `.find_best_choices(&items, priority) -> Vec<Item>` | ported | arbitrary item list → edible, non-`banned_food` entries sorted by the priority descending, ties keeping the input order (upstream's stable sort); `eat()`'s fallback is `[0]` of this over `getAllItems` |
| `autoEat.enableAuto()/disableAuto()` | pathfinder | `.enable_auto()` / `.disable_auto()` | ported | spawns/retires the tick driver (selects on `bot.shutdown`, tolerates `Lagged`, exits on `Closed`/shutdown); an in-flight eat finishes normally on disable |
| `autoEat.cancelEat()` | pathfinder | `.cancel_eat()` | ported | fails the waiting eat with `Eating manually canceled!` and deactivates the item; no-op when idle or before the eat's wait begins — upstream's `_rejectionBinding` exists only during `buildEatingListener`, so an equip-window cancel is a no-op there too |
| `autoEat.isEating` / `.enabled` | pathfinder | `.is_eating()` / `.enabled()` | ported | |
| `autoEat.foods / foodsArray / foodsByName` | pathfinder | `AutoEat::food_data(kind) -> Option<FoodData>` | diverges | per-kind lookup instead of registry tables — same source data (vanilla `food` component: `nutrition`, `saturation`, `can_always_eat`); enumerate by mapping over `ItemKind` if a list is needed |
| `autoEat.setOpts` | pathfinder | `.set_opts(AutoEatOpts)` | ported | whole-struct replace; read `opts()`, edit fields, write back for the upstream partial-merge form |
| `autoEat` events `eatStart`/`eatFinish`/`eatFail` | pathfinder | `BotEvent::EatStart{food, offhand, equip_old_item}` / `EatFinish{food, offhand, equip_old_item}` / `EatFail(String)` | diverges | emitted on the shared `BotEvent` stream (upstream fires them on the `bot.autoEat` emitter); `EatFail` carries the error message, not the `Error` object, and — mirroring upstream's emit-after-throw — fires only when `strict_errors` is off (unreachable under the default `true`); `EatFinish` fires on failure too, upstream's `finally` |

## External plugin ecosystem (out of scope)

Tracked for completeness — these are third-party npm packages, not part of the mineflayer crate API. The port substitutes Rust-side equivalents only where PORT-PLAN assigns them (pathfinder, collectBlock, pvp, tool, autoEat).

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `mineflayer-autocrystal` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-statemachine` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `MineflayerArmorManager` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-web-inventory` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-dashboard` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `minecraftHawkEye` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-GUI` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-projectile` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-movement` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-radar` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `MineflayerAutoAuth` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-bloodhound` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-tps` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-panorama` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-death-event` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `minecraft-mcp-server` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `prismarine-viewer` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-navigate` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |
| `mineflayer-builder` | misc | `n/a` | wontfix | external plugin; outside port scope (see PORT-PLAN) |

## Additive Rust surface

Public API beyond the upstream inventory — listed so the ledger stays complete; none of these has an upstream row by design.

| Rust API | domain | status | notes |
|---|---|---|---|
| `bot.events()` | lifecycle | additive | `broadcast::Receiver<BotEvent>` subscription — the `bot.on`/`once` mechanism every event row implies. The first call returns a receiver subscribed before the event bridge started, so `Init`/other startup events are not lost; derived world/entity/window events are decoded inside the bridge's event loop, so they land in packet order and can't be dropped by a lagging consumer |
| `bot.equip_kind(ItemKind, EquipDest)` | inventory | additive | the `equip(itemId, dest)` arm of upstream `equip`; the `bot.equip` row names only `equip(&Item, dest)` |
| `bot.find_inventory_item(kind)` | inventory | additive | bot-level shortcut over `bot.inventory().find_item_range`; upstream's `window.findInventoryItem` is prismarine-windows internal |
| `bot.equipment(entity)` | entity | additive | per-entity `EquipmentSlot -> ItemStack` map; no upstream analog (named inside the `Entity` row note) |
| `bot.known_recipe_count()` | craft | additive | recipe-book introspection; no upstream analog |
| `EnchantmentTable::is_ready()` | inventory | additive | readiness flag behind the `enchant` gate / 'ready' event |
| `Window` helpers | inventory | additive | `id`, `menu`, `menu_kind`, `title`, `is_open`, `slot`, `selected_item`, `player_range`, `container_range`, `container_items`, `inventory_items`, `find_item_range`, `first_empty_slot_range`, `empty_slot_count` — port helpers surfaced `pub` |
| `Item` helpers | inventory | additive | `of`, `stack_size`, `durability_used`, `max_durability`, `custom_data` — extra accessors over the ported `Item` row |
| `BotEvent::Packet` | lifecycle | additive | raw `ClientboundGamePacket` escape hatch; upstream analog is the internal `_client.on('packet')` |
| `Settings.locale` | misc | additive | `bot.settings().locale`; upstream tracks locale via `ClientOptions`, not `bot.settings` |
| `bot.tool().equip_for_entity(&entity)` | pathfinder | additive | generic max-`attack_damage` hotbar pick before an attack — no `equipForEntity` exists upstream (also named inside the `tool.*` row) |

<!-- 526 rows: 496 upstream-inventory + 19 external-plugin + 11 additive -->
