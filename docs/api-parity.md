# mineflayer -> mineflayer-rs API parity table

Binding spec: every public API row from `docs/surveys/mineflayer-api.md`, grouped by domain.
Status ∈ `pending` (not ported), `ported`, `diverges` (intentional semantic difference), `wontfix`.

Global divergence (standing order 11): **azalea tracks a single MC version** (crates.io azalea `0.16.0+mc26.1` -> MC `26.1`).
Every version-selection row is `diverges` — single-version, latest-only. Noted once here, not per row.

Naming: Rust `snake_case`, `Promise<T>` -> `async fn -> Result<T>` (or a stream/event where noted).
Events become `BotEvent` variants or per-domain event enums; payloads use azalea types.

## Module exports (`require('mineflayer')`)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `createBot(options)` | module | `Bot::builder() -> BotBuilder; BotBuilder::connect() -> Result<Bot>` | ported | builder style; connect runs bot on a dedicated thread (azalea LocalSet) |
| `Location` | module | `struct Location` | pending | legacy helper; azalea BlockPos/Vec3 cover most uses |
| `Painting` | module | `struct Painting` | pending | entity metadata wrapper |
| `ScoreBoard (class)` | module | `mineflayer::ScoreBoard` | ported | see Scoreboard section |
| `BossBar (class)` | module | `mineflayer::BossBar` | ported | see Scoreboard section |
| `Particle (class)` | module | `mineflayer::Particle` | ported | see Particles section |
| `testedVersions` | module | `n/a` | diverges | azalea tracks latest MC only (standing order 11); no multi-version list |
| `latestSupportedVersion` | module | `const MC_VERSION: &str` | diverges | single-version: equals the azalea-targeted MC version |
| `oldestSupportedVersion` | module | `n/a` | diverges | single-version: same as latest |
| `supportFeature(feature, version)` | module | `pub fn support_feature(name) -> bool` | diverges | no version arg — answers for the single supported MC version |
| `mcdata.blocks` | module | `azalea::registry / azalea::block` | pending | re-export azalea registry |
| `mcdata.items` | module | `azalea::registry / azalea_inventory::item` | pending | re-export azalea registry |
| `mcdata.materials` | module | `own lookup table (dig-speed materials)` | pending | minecraft-data 'materials' have no azalea analog; tool-speed data |
| `mcdata.recipes` | module | `recipe registry (custom over c_update_recipes)` | pending | tier-2 packet bridge stores recipes |
| `mcdata.instruments` | module | `enum Instrument` | pending | note-block instruments |
| `mcdata.biomes` | module | `azalea::registry::data / builtin::Biome` | pending | — |
| `mcdata.entities` | module | `azalea::registry::builtin::EntityKind` | pending | — |
| `Vec3 (prismarine)` | module | `azalea::Vec3 (re-export)` | pending | azalea_core::position::Vec3 |
| `Entity (prismarine-entity)` | module | `azalea::EntityRef` + `EntityExt` trait | diverges | EntityExt covers look/velocity/onGround/username/skin only; uuid/kind/attributes/effects/metadata need raw `get_component`, equipment needs `bot.equipment` |
| `Block (prismarine-block)` | module | `mineflayer::world::Block { pos, state, world }` | diverges | name/kind/state_id/bounding_box/diggable/property/behavior ported; no blockEntity/getSignText (azalea has no block-entity storage — U11) |
| `Biome (prismarine-biome)` | module | `azalea::registry Biome` | pending | — |
| `Item (prismarine-item)` | module | `azalea_inventory::ItemStack wrapper` | pending | — |
| `Recipe (prismarine-recipe)` | module | `own Recipe type` | pending | — |
| `ChatMessage (prismarine-chat)` | module | `azalea::chat::FormattedText wrapper` | pending | mc26.1: azalea::chat is azalea_client::plugins::chat |
| `Window (prismarine-windows)` | module | `Window trait + azalea ContainerHandle` | pending | see Window classes |

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
| `options.logErrors` | lifecycle | `BotOptions::log_errors` | pending | — |
| `options.hideErrors` | lifecycle | `BotOptions::hide_errors` | pending | — |
| `options.keepAlive` | lifecycle | `n/a` | wontfix | n/a — azalea owns keepalive; `Event::KeepAlive` dropped by the bridge |
| `options.checkTimeoutInterval` | lifecycle | `BotOptions::check_timeout_interval` | pending | — |
| `options.loadInternalPlugins` | lifecycle | `n/a` | wontfix | n/a — compile-time plugin model |
| `options.storageBuilder` | lifecycle | `n/a` | wontfix | n/a — anvil provider out of scope |
| `options.client` | lifecycle | `Bot::new(client)` | ported | doc-hidden escape hatch; no event bridge on prebuilt clients |
| `options.brand` | lifecycle | `ClientInformation brand field` | pending | — |
| `options.respawn` | lifecycle | `n/a` | diverges | azalea auto-respawns unconditionally; no toggle exposed |
| `options.plugins` | lifecycle | `compile-time module set` | pending | no runtime plugin loader |
| `options.physicsEnabled` | lifecycle | `BotOptions::physics_enabled` | pending | — |
| `options.chat (settings passthrough)` | lifecycle | `BotOptions -> ClientInformation` | pending | — |
| `options.colorsEnabled` | lifecycle | `ClientInformation::chat_colors` | pending | — |
| `options.viewDistance` | lifecycle | `BotBuilder::view_distance(u8)` | ported | applied on `Event::Init` so login already carries it |
| `options.difficulty` | lifecycle | `ClientInformation (no difficulty field upstream)` | pending | stored setting only; server-authoritative |
| `options.skinParts` | lifecycle | `ClientInformation::model_customization` | pending | — |
| `options.enableTextFiltering` | lifecycle | `ClientInformation::text_filtering_enabled` | pending | — |
| `options.enableServerListing` | lifecycle | `ClientInformation::allows_listing` | pending | — |
| `options.particleStatus` | lifecycle | `ClientInformation::particle_status` | pending | — |
| `options.chatLengthLimit` | lifecycle | `BotOptions::chat_length_limit` | pending | — |
| `options.defaultChatPatterns` | lifecycle | `BotOptions::default_chat_patterns` | pending | — |
| `options.mainHand` | lifecycle | `ClientInformation::main_hand` | pending | typed-only in mineflayer |
| `options.maxCatchupTicks` | lifecycle | `BotOptions::max_catchup_ticks` | pending | typed-only in mineflayer |
| `options ClientOptions passthrough` | lifecycle | `JoinOpts (proxies, custom addr)` | pending | realms/profilesFolder/session/connect/agent/stream |

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
| `bot.supportFeature(name)` | lifecycle | `bot.support_feature(name) -> bool` | diverges | single-version semantics (standing order 11) |
| `bot.username` | lifecycle | `bot.username() -> String` | ported | GameProfileComponent name; '' before login |
| `bot.version` | lifecycle | `bot.version() -> &'static str` | diverges | returns the single supported MC version (azalea mc26.1) |
| `bot.protocolVersion` | lifecycle | `bot.protocol_version() -> i32` | ported | protocol number of the single supported version (775) |
| `bot.majorVersion` | lifecycle | `bot.major_version() -> &'static str` | ported | same as `version()` — single-version port |
| `bot.registry` | lifecycle | `bot.registry() -> RegistryHolder` | ported | azalea dynamic registries snapshot |
| `bot._client` | lifecycle | `bot.client() -> &azalea::Client` | ported | unstable upstream; public escape hatch here |
| `bot._warn(...)` | lifecycle | `n/a` | wontfix | n/a — tracing/logging, not an API |
| `bot._getDimensionName() (internal)` | lifecycle | `pub(crate) helpers — internal` | pending | dimension-name lookup |
| `event('inject_allowed')` | lifecycle | `BotEvent::Init` | ported | azalea Event::Init analog |
| `event('login')` | lifecycle | `BotEvent::Login` | ported | — |
| `event('spawn')` | lifecycle | `BotEvent::Spawn` | ported | first azalea Spawn |
| `event('respawn')` | lifecycle | `BotEvent::Respawn` | ported | synthesized: emitted before every `Spawn` after the first |
| `event('connect') (undoc)` | lifecycle | `BotEvent::Connected?` | pending | — |
| `event('kicked')` | lifecycle | `BotEvent::Kicked(String)` | diverges | azalea folds kick into `Disconnect(reason)`; we emit Kicked then End; no loggedIn flag |
| `event('end')` | lifecycle | `BotEvent::End(String)` | ported | exactly-once; reason = quit arg / kick / 'socketClosed' |
| `event('error')` | lifecycle | `BotEvent::Error(String)` | ported | currently: connection failures only |

## Chat / messaging

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.chat(message)` | chat | `bot.chat(&str) -> Result<()>` | ported | /-prefixed -> command; >256 split like upstream (newline-aware, header counts) |
| `bot.whisper(user, msg)` | chat | `bot.whisper(&str, &str) -> Result<()>` | ported | `/tell user ` header counts against the 256 limit; upstream uses /tell only (no /msg fallback) |
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
| `bot.findBlocks(options)` | world | `bot.find_blocks(FindBlocksOpts) -> Vec<BlockPos>` | ported | euclid closest-first; defaults max_distance=16, count=1; chunk-layer early exit |
| `bot.findBlock(options)` | world | `bot.find_block(FindBlocksOpts) -> Option<Block>` | ported | — |
| `bot.updateSign(block, text, back)` | world | `bot.update_sign(&block, text, back) -> Result<()>` | diverges | write-only: sends s_sign_update (front/back); sign text unreadable (no block-entity storage — U11) |
| `bot.world` | world | `bot.world() -> Option<WorldRef>` | diverges | WorldRef wraps the client (parking_lot lock type isn't nameable); get_block_state/block_at/raycast/find_blocks/set_block_state |
| `bot.spawnPoint` | world | `bot.spawn_point() -> BlockPos` | ported | c_set_default_spawn_position decoded in packet bridge |
| `bot._updateBlockState (internal)` | world | `n/a` | wontfix | azalea applies block updates to the world itself |
| `bot._getBlockAtEyeLevel (internal)` | world | `n/a` | wontfix | internal helper; eye raycasts go through `block_at_cursor` |
| `event('blockUpdate')` | world | `BotEvent::BlockUpdate { old, new }` | ported | c_block_update + c_section_blocks_update decoded; old=None when chunk unloaded |
| `event('blockUpdate:(x,y,z)')` | world | `match` on `BotEvent::BlockUpdate`'s `new.pos` | diverges | no per-position emitter; subscribers filter |
| `event('blockPlaced')` | world | `BotEvent::BlockPlaced{old,new}` | pending | bot's own placements |
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
| `bot.swingArm(hand, showHand)` | entity | `bot.swing_arm(hand)` | diverges | `showHand` has no wire meaning in this protocol (s_swing always carries hand) |
| `bot.activateEntity(e)` | entity | `bot.activate_entity(e)` | ported | azalea entity_interact; no await needed (fire-and-forget like upstream); azalea double-sends `s_interact` (attack+interact arms) vs upstream's one |
| `bot.activateEntityAt(e, pos)` | entity | `bot.activate_entity_at(e, pos)` | ported | EntityInteractEvent with explicit location |
| `bot.useOn(target)` | entity | `bot.use_on(e)` | ported | same interact packet as activateEntity, like upstream; azalea double-sends `s_interact` vs upstream's one |
| `bot.mount(e)` | entity | `bot.mount(e)` | ported | same interact packet as upstream's useEntity(target,0); azalea double-sends `s_interact` vs upstream's one |
| `bot.dismount()` | entity | `bot.dismount()` | diverges | sends shift-press player_input (the real dismount); upstream's `jump:true` literal looks like an upstream bug |
| `bot.moveVehicle(left, fwd)` | entity | `bot.move_vehicle(left:i8, fwd:i8)` | diverges | sends raw player_input like upstream, but doesn't integrate with azalea's PhysicsState |
| `bot.getExplosionDamages(e,pos,r,raw)` | entity | `bot.get_explosion_damages(..) -> Option<f32>` | diverges | exposure raycasts loaded chunks (Collider shapes); armor from tracked c_update_attributes → None when unknown, like upstream's null |
| `bot.elytraFly()` | entity | `bot.elytra_fly() -> Result<()>` | diverges | same precondition errors; elytra-equipped check deferred to inventory unit |
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
| `Player.profileKeys` | entity | `player.profile_keys()` | pending | publicKey+signature |
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
| `event('entityCrouch')` | entity | `BotEvent::EntityCrouch(EntityRef)` | ported | metadata shift-key edge detection |
| `event('entityUncrouch')` | entity | `BotEvent::EntityUncrouch(EntityRef)` | ported | — |
| `event('entityEquip')` | entity | `BotEvent::EntityEquip(EntityRef)` | ported | c_set_equipment decoded ourselves (azalea logs-and-drops it) |
| `event('entitySleep')` | entity | `BotEvent::EntitySleep(EntityRef)` | ported | pose/sleeping-pos metadata edge |
| `event('entityElytraFlew')` | entity | `BotEvent::EntityElytraFlew(EntityRef)` | ported | fall-flying metadata edge |
| `event('entityAttach')` | entity | `BotEvent::EntityAttach{entity,vehicle}` | diverges | driven by c_set_entity_link (leashes); upstream also uses attach_entity for riders — passengers tracked separately (Mounted/Dismounted) |
| `event('entityDetach')` | entity | `BotEvent::EntityDetach{entity,vehicle}` | diverges | see entityAttach |
| `event('entityEffect')` | entity | `BotEvent::EntityEffect{entity,effect,data}` | ported | c_update_mob_effect |
| `event('entityEffectEnd')` | entity | `BotEvent::EntityEffectEnd{entity,effect}` | ported | c_remove_mob_effect |
| `event('entityPlaced') (undoc)` | entity | — | wontfix | emitted by placeEntity (block placement) — owned by the block-place unit |
| `event('itemDrop')` | entity | `BotEvent::ItemDrop(EntityRef)` | ported | item-stack metadata entry, like upstream |
| `event('playerCollect')` | entity | `BotEvent::PlayerCollect{collector,collected}` | ported | c_take_item_entity |
| `event('playerJoined')` | entity | `BotEvent::PlayerJoined(PlayerInfo)` | ported | azalea Event::AddPlayer |
| `event('playerUpdated')` | entity | `BotEvent::PlayerUpdated(PlayerInfo)` | ported | azalea Event::UpdatePlayer |
| `event('playerLeft')` | entity | `BotEvent::PlayerLeft(PlayerInfo)` | ported | azalea Event::RemovePlayer |
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
| `event('mount')` | movement | `BotEvent::Mounted{vehicle: Option<EntityRef>}` | ported | c_set_passengers diff in the entity tracker (single decoder; azalea tracks no vehicle component) |
| `event('dismount')` | movement | `BotEvent::Dismounted{vehicle: Option<EntityRef>}` | ported | c_set_passengers diff + vehicle-despawn path (entity tracker) |
| `event('physicsTick')` | movement | `BotEvent::Tick` | ported | azalea Event::Tick |
| `event('physicTick') (dep)` | movement | `alias of physicsTick` | ported | deprecated; same BotEvent::Tick |
| `event('abilities')` | movement | `BotEvent::Abilities(abilities)` | ported | on ClientboundPlayerAbilities packet |

## Inventory / containers / windows

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.equip(item, dest)` | inventory | `bot.equip(item, dest).await -> Result<()>` | pending | — |
| `bot.unequip(dest)` | inventory | `bot.unequip(dest).await` | pending | — |
| `bot.tossStack(item)` | inventory | `bot.toss_stack(item).await` | pending | — |
| `bot.toss(type, meta, count)` | inventory | `bot.toss(kind, count).await` | pending | metadata -> data components |
| `bot.setQuickBarSlot(slot)` | inventory | `bot.set_quick_bar_slot(u8)` | pending | azalea set_selected_hotbar_slot |
| `bot.getEquipmentDestSlot(dest)` | inventory | `bot.equipment_dest_slot(dest) -> u16` | pending | — |
| `bot.consume()` | inventory | `bot.consume().await` | pending | no azalea helper; use-item + wait |
| `bot.activateItem(offHand)` | inventory | `bot.activate_item(hand)` | pending | azalea start_use_item |
| `bot.deactivateItem()` | inventory | `bot.deactivate_item()` | pending | release-use packet; no public helper upstream |
| `bot.simpleClick.leftMouse(slot)` | inventory | `bot.simple_click_left(slot).await` | pending | — |
| `bot.simpleClick.rightMouse(slot)` | inventory | `bot.simple_click_right(slot).await` | pending | — |
| `bot.clickWindow(slot,btn,mode)` | inventory | `bot.click_window(..).await` | pending | ClickOperation mapping |
| `bot.putSelectedItemRange(..)` | inventory | `bot.put_selected_item_range(..).await` | pending | — |
| `bot.putAway(slot)` | inventory | `bot.put_away(slot).await` | pending | — |
| `bot.closeWindow(window)` | inventory | `window.close() / bot.close_window()` | pending | — |
| `bot.transfer(options)` | inventory | `bot.transfer(opts).await` | pending | — |
| `bot.openBlock(block, dir, cursor)` | inventory | `bot.open_block(block).await -> Window` | pending | azalea open_container_at |
| `bot.openEntity(entity, Class?)` | inventory | `bot.open_entity(e).await -> Window` | pending | — |
| `bot.moveSlotItem(src, dst)` | inventory | `bot.move_slot_item(a, b).await` | pending | — |
| `bot.updateHeldItem()` | inventory | `bot.update_held_item()` | pending | — |
| `bot.openContainer(b\|e, ..)` | inventory | `bot.open_container(..).await` | pending | chest/dispenser/etc. |
| `bot.openChest(..) (dep)` | inventory | `alias of openContainer` | pending | — |
| `bot.openFurnace(block)` | inventory | `bot.open_furnace(b).await -> Furnace` | pending | — |
| `bot.openDispenser(..) (dep)` | inventory | `alias of openContainer` | pending | — |
| `bot.openEnchantmentTable(b)` | inventory | `bot.open_enchantment_table(b).await` | pending | — |
| `bot.openAnvil(b)` | inventory | `bot.open_anvil(b).await` | pending | — |
| `bot.openVillager(e)` | inventory | `bot.open_villager(e).await` | pending | 'ready' event; merchant offers need bridge |
| `bot.trade(villager, idx, times?)` | inventory | `villager.trade(idx, count).await` | pending | s_select_trade + click choreography |
| `bot.writeBook(slot, pages)` | inventory | `bot.write_book(slot, pages).await` | pending | s_edit_book exists upstream |
| `bot.signBook(slot,pages,author,title) (undoc)` | inventory | `bot.sign_book(..).await` | pending | — |
| `bot.setCommandBlock(pos, cmd, opts)` | inventory | `bot.set_command_block(..)` | pending | — |
| `bot.inventory` | inventory | `bot.inventory() -> Window` | pending | player inventory window |
| `bot.quickBarSlot` | inventory | `bot.quick_bar_slot() -> u8` | pending | — |
| `bot.heldItem` | inventory | `bot.held_item() -> Option<Item>` | pending | — |
| `bot.usingHeldItem` | inventory | `bot.using_held_item() -> bool` | pending | — |
| `bot.currentWindow (undoc)` | inventory | `bot.current_window() -> Option<Window>` | pending | — |
| `bot.QUICK_BAR_START (undoc)` | inventory | `const QUICK_BAR_START: usize` | pending | — |
| `Window.items()` | inventory | `window.items() -> Vec<Item>` | pending | — |
| `Window.slots` | inventory | `window.slots() -> &[Slot]` | pending | — |
| `Window.deposit(type,meta,count,nbt)` | inventory | `window.deposit(..).await` | pending | — |
| `Window.withdraw(type,meta,count,nbt)` | inventory | `window.withdraw(..).await` | pending | — |
| `Window.close()` | inventory | `window.close().await` | pending | ContainerHandle::close |
| `Chest / Container class` | inventory | `struct Chest` | pending | generic container; deposit/withdraw/close |
| `Dispenser class` | inventory | `struct Dispenser` | pending | same as Chest |
| `window event 'open'` | inventory | `WindowEvent::Open` | pending | — |
| `window event 'close'` | inventory | `WindowEvent::Close` | pending | — |
| `window event 'updateSlot'` | inventory | `WindowEvent::UpdateSlot{slot,old,new}` | pending | — |
| `Furnace class` | inventory | `struct Furnace` | pending | — |
| `Furnace.fuel` | inventory | `furnace.fuel() -> f32` | pending | — |
| `Furnace.progress` | inventory | `furnace.progress() -> f32` | pending | — |
| `Furnace.takeInput()` | inventory | `furnace.take_input().await -> Item` | pending | — |
| `Furnace.takeFuel()` | inventory | `furnace.take_fuel().await` | pending | — |
| `Furnace.takeOutput()` | inventory | `furnace.take_output().await` | pending | — |
| `Furnace.putInput(type,meta,count)` | inventory | `furnace.put_input(..).await` | pending | — |
| `Furnace.putFuel(...)` | inventory | `furnace.put_fuel(..).await` | pending | — |
| `Furnace.inputItem()` | inventory | `furnace.input_item() -> Item` | pending | — |
| `Furnace.fuelItem()` | inventory | `furnace.fuel_item()` | pending | — |
| `Furnace.outputItem()` | inventory | `furnace.output_item()` | pending | — |
| `furnace event 'update'` | inventory | `FurnaceEvent::Update` | pending | — |
| `EnchantmentTable class` | inventory | `struct EnchantmentTable` | pending | — |
| `EnchantmentTable.xpseed` | inventory | `enchant.xpseed()` | pending | c_container_set_data; azalea stores generic data only |
| `EnchantmentTable.enchantments` | inventory | `enchant.enchantments() -> [Option<Enchant>;3]` | pending | — |
| `EnchantmentTable.targetItem()` | inventory | `enchant.target_item()` | pending | — |
| `EnchantmentTable.enchant(choice)` | inventory | `enchant.enchant(0..2).await` | pending | s_container_button_click |
| `EnchantmentTable.takeTargetItem()` | inventory | `enchant.take_target_item().await` | pending | — |
| `EnchantmentTable.putTargetItem(item)` | inventory | `enchant.put_target_item(..).await` | pending | — |
| `EnchantmentTable.putLapis(item)` | inventory | `enchant.put_lapis(..).await` | pending | — |
| `enchant event 'ready'` | inventory | `EnchantmentEvent::Ready` | pending | — |
| `Anvil class` | inventory | `struct Anvil` | pending | not a Window subclass upstream |
| `Anvil.combine(a,b,name?)` | inventory | `anvil.combine(..).await` | pending | click choreography + s_rename_item |
| `Anvil.rename(item,name) (undoc)` | inventory | `anvil.rename(..).await` | pending | — |
| `Villager class` | inventory | `struct Villager` | pending | Merchant menu; offers need c_merchant_offers bridge |
| `Villager.trades` | inventory | `villager.trades() -> &[VillagerTrade]` | pending | — |
| `Villager.selectedTrade` | inventory | `villager.selected_trade()` | pending | — |
| `Villager.trade(index,count?)` | inventory | `villager.trade(idx,count).await` | pending | — |
| `villager event 'ready'` | inventory | `VillagerEvent::Ready` | pending | — |
| `event('windowOpen')` | inventory | `BotEvent::WindowOpen(window)` | pending | MenuOpenedEvent |
| `event('windowClose')` | inventory | `BotEvent::WindowClose(window)` | pending | — |
| `event('heldItemChanged')` | inventory | `BotEvent::HeldItemChanged(Option<Item>)` | pending | — |
| `bot._setSlot (internal)` | inventory | `pub(crate) helpers — internal` | pending | inventory slot state update |
| `bot._syncWindow (internal)` | inventory | `pub(crate) helpers — internal` | pending | window transaction sync |
| `bot._ensureHasSentCarriedItem (internal)` | inventory | `pub(crate) helpers — internal` | pending | cursor/carried item sync |
| `event confirmTransaction (internal)` | inventory | `internal` | pending | window transaction plumbing |
| `event setSlot:{windowId} (internal)` | inventory | `internal` | pending | — |
| `event setWindowItems:{windowId} (internal)` | inventory | `internal` | pending | — |

## Crafting / recipes

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.recipesFor(type,meta,min,table)` | craft | `bot.recipes_for(..) -> Vec<Recipe>` | pending | craftable with current inventory |
| `bot.recipesAll(type,meta,table)` | craft | `bot.recipes_all(..) -> Vec<Recipe>` | pending | — |
| `bot.craft(recipe, count?, table?)` | craft | `bot.craft(recipe, n, table).await` | pending | s_place_recipe packet exists upstream |
| `Recipe class` | craft | `struct Recipe` | pending | prismarine-recipe analog |

## Block interaction (dig / place / activate)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.dig(block, forceLook, digFace)` | interact | `bot.dig(block).await` | pending | azalea Client::mine; events on finish/abort |
| `bot.stopDigging()` | interact | `bot.stop_digging()` | pending | remove Mining / abort |
| `bot.digTime(block)` | interact | `bot.dig_time(block) -> Duration` | pending | azalea get_mine_progress |
| `bot.canDigBlock(block)` | interact | `bot.can_dig_block(block) -> bool` | pending | — |
| `bot.placeBlock(ref, faceVec)` | interact | `bot.place_block(block, dir).await` | pending | block_interact + prediction gap |
| `bot.placeEntity(ref, faceVec)` | interact | `bot.place_entity(..).await -> EntityRef` | pending | armor stand/end crystal |
| `bot.activateBlock(b, dir, cursor)` | interact | `bot.activate_block(b).await` | pending | azalea block_interact |
| `bot._nextSequence (internal)` | interact | `pub(crate) helpers — internal` | pending | server action-sequence counter (dig/place) |
| `bot.targetDigBlock (undoc)` | interact | `bot.target_dig_block() -> Option<Block>` | pending | — |
| `bot.targetDigFace (undoc)` | interact | `bot.target_dig_face()` | pending | — |
| `bot.lastDigTime (undoc)` | interact | `bot.last_dig_time()` | pending | — |
| `bot._genericPlace (internal)` | interact | `pub(crate) helper` | pending | — |
| `bot._placeBlockWithOptions (internal)` | interact | `pub(crate) helper` | pending | — |
| `bot._placeEntityWithOptions (internal)` | interact | `pub(crate) helper` | pending | — |
| `event('diggingCompleted')` | interact | `BotEvent::DiggingCompleted(block)` | pending | azalea FinishMiningBlockEvent |
| `event('diggingAborted')` | interact | `BotEvent::DiggingAborted(block)` | pending | — |

## Scoreboard / teams / bossbar / title

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.scoreboards` | misc | `bot.scoreboards() -> HashMap<String,ScoreBoard>` | ported | cloned snapshot; reset each login |
| `bot.scoreboard` | misc | `bot.scoreboard(ScoreboardSlot) -> Option<ScoreBoard>` | diverges | slot-enum accessor instead of a slot-keyed map |
| `bot.teams` | misc | `bot.teams() -> HashMap<String,Team>` | ported | cloned snapshot |
| `bot.teamMap` | misc | `bot.team_map() -> HashMap<String,Team>` | ported | cloned snapshot |
| `bot.bossBars (undoc)` | misc | `bot.boss_bars() -> Vec<BossBar>` | ported | — |
| `ScoreBoard.name` | misc | `scoreboard.name` (field) | ported | — |
| `ScoreBoard.title` | misc | `scoreboard.title` (field) | ported | `FormattedText`, not pre-rendered |
| `ScoreBoard.itemsMap` | misc | `scoreboard.items_map() -> &HashMap` | ported | — |
| `ScoreBoard.items` | misc | `scoreboard.items() -> Vec<&ScoreItem>` | ported | name-sorted like upstream |
| `ScoreBoard.setTitle(t)` | misc | `scoreboard.set_title(FormattedText)` | ported | client-side only, like upstream |
| `ScoreBoard.add(name,value)` | misc | `scoreboard.add(name, value)` | ported | — |
| `ScoreBoard.remove(name)` | misc | `scoreboard.remove(&str)` | ported | — |
| `Team fields` | misc | `team.{name,display_name,friendly_fire,see_friendly_invisibles,nametag_visibility,collision_rule,color,prefix,suffix,members}` | ported | color is `ChatFormatting` (Reset = none) |
| `Team.parseMessage` | misc | `n/a` | diverges | upstream internal helper; packet reducer decodes directly |
| `Team.add / remove / update` | misc | `n/a` | diverges | upstream internal helpers; state mutates via packets only |
| `Team.displayName(member)` | misc | `team.member_display_name(member) -> FormattedText` | ported | prefix + colored member + suffix |
| `BossBar fields` | misc | `bossbar.{entity_uuid,title,health,dividers,color,darken_sky,dragon_bar,create_fog}` | ported | `entity_uuid` is the bar uuid string; `flags` folded into the three bools |
| `event('scoreboardCreated')` | misc | `BotEvent::ScoreboardCreated(sb)` | ported | — |
| `event('scoreboardDeleted')` | misc | `BotEvent::ScoreboardDeleted(sb)` | ported | carries the removed board — upstream emits before deleting, same object |
| `event('scoreboardTitleChanged')` | misc | `BotEvent::ScoreboardTitleChanged(sb)` | ported | — |
| `event('scoreUpdated')` | misc | `BotEvent::ScoreUpdated{scoreboard,item}` | ported | — |
| `event('scoreRemoved')` | misc | `BotEvent::ScoreRemoved{scoreboard,item}` | diverges | no scan-all-boards fallback when the named objective is absent, and nothing emitted when the owner is missing (upstream emits `undefined` item) |
| `event('scoreboardPosition')` | misc | `BotEvent::ScoreboardPosition{slot,scoreboard}` | ported | — |
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
| `bot.game.dimension` | state | `bot.dimension() -> Option<WorldName>` | ported | top-level `bot.dimension()`; `bot.game()` facade deferred |
| `bot.game.difficulty` | state | `bot.game().difficulty` | ported | c_change_difficulty decoded |
| `bot.game.gameMode` | state | `bot.game_mode() -> Option<GameMode>` | ported | top-level `bot.game_mode()`; `bot.game()` facade deferred |
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
| `event('death')` | state | `BotEvent::Death` | ported | azalea Event::Death (payload packet dropped) |
| `event('breath')` | state | `BotEvent::Breath` | ported | AirSupply tick diff |
| `event('experience')` | state | `BotEvent::Experience` | ported | emitted per `set_experience` packet |
| `event('spawnReset')` | state | `BotEvent::SpawnReset` | ported | `game_event` NoRespawnBlockAvailable |

## Bed / sleep

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.sleep(bedBlock)` | misc | `bot.sleep(BlockPos).await` | diverges | same flow minus upstream's client-side night/monster/occupancy checks (server-enforced) |
| `bot.wake()` | misc | `bot.wake()` | diverges | sync send of `s_player_command` StopSleeping (upstream's await only wraps packet write) |
| `bot.isABed(bedBlock)` | misc | `bot.is_a_bed(BlockState) -> bool` | ported | takes a block *state* like upstream's block.type check |
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
| `bot.setSettings(options)` | misc | `bot.set_settings(Settings)` | ported | merge via `Settings { field, ..bot.settings() }`; rejects view_distance == 0 |
| `bot.settings.chat` | misc | `bot.settings().chat_visibility` | ported | `ChatVisibility` enum |
| `bot.settings.colorsEnabled` | misc | `bot.settings().chat_colors` | ported | — |
| `bot.settings.viewDistance` | misc | `bot.settings().view_distance` | diverges | chunks; azalea default is 8, upstream default `'far'` is 12 |
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
| `bot.creative.setInventorySlot(slot,item)` | inventory | `bot.creative().set_inventory_slot(slot,item).await` | ported | waits for `container_set_slot` echo; rejects after 5s like upstream |
| `bot.creative.clearSlot(slot)` | inventory | `bot.creative().clear_slot(slot).await` | ported | — |
| `bot.creative.clearInventory()` | inventory | `bot.creative().clear_inventory().await` | ported | — |
| `bot.creative.flyTo(dest)` | inventory | `bot.creative().fly_to(Vec3).await` | ported | straight-line 0.5-block steps per 50ms |
| `bot.creative.startFlying()` | inventory | `bot.creative().start_flying()` | ported | sets `PlayerAbilities.flying` + s_player_abilities |
| `bot.creative.stopFlying()` | inventory | `bot.creative().stop_flying()` | ported | — |

## Plugin ecosystem (in port scope — U7/U12)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.pathfinder` | pathfinder | `bot.pathfinder() -> Pathfinder<'_>` | ported | namespace handle over `PathfinderClientExt` |
| `pathfinder.goto(goal)` | pathfinder | `pf.goto(Goal) -> Result<()>` | ported | resolves on `goal_reached`; `Err` on `path_stop`/disconnect; an already-satisfied goal resolves immediately |
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
| `GoalGetToBlock` | pathfinder | `Goal::GetToBlock(BlockPos)` | ported | `ReachBlockPosGoal` (line-of-sight/reach against live chunks) |
| `GoalPlaceBlock` | pathfinder | — | wontfix | azalea's pathfinder never places blocks; no standable-position goal exists to wrap |
| `GoalXZ` | pathfinder | `Goal::XZ{x, z}` | ported | `XZGoal` |
| `GoalY` | pathfinder | `Goal::Y{y}` | ported | `YGoal` |
| `GoalInvert` | pathfinder | `Goal::Invert(Box<Goal>)` | diverges | custom goal: negated success + negated heuristic — same semantics (and same A* pathology) as azalea's deprecated `InverseGoal` |
| `GoalFollow` | pathfinder | `Goal::Follow{entity, range}` | diverges | watcher task re-issues `RadiusGoal` when the entity's block position changes and reports `path_stop` on despawn; no continuous moving goal like upstream |
| `GoalCompositeAny` | pathfinder | `Goal::Any(Vec<Goal>)` | ported | boxed `OrGoals` — any-success, min heuristic |
| `GoalCompositeAll` | pathfinder | `Goal::All(Vec<Goal>)` | ported | boxed `AndGoals` — all-success, max heuristic |
| `setGoal(g, dynamic=true)` | pathfinder | `pf.set_goal(goal, true)` | diverges | watcher re-issues the goal each tick it stops holding instead of upstream's always-re-evaluating goal; `goal_reached` is still suppressed while the goal holds |
| `event('goal_reached')` | pathfinder | `BotEvent::GoalReached` | ported | emitted when azalea clears the goal while its success check holds; suppressed for `dynamic` goals like upstream |
| `event('path_update')` | pathfinder | `BotEvent::PathUpdate{path, is_partial}` | ported | per-tick diff of `Pathfinder`/`ExecutingPath` (azalea's `PathFoundEvent` never reaches `Event`); also fires on failed calcs, matching upstream's per-result `path_update` |
| `event('goal_updated')` | pathfinder | `BotEvent::GoalUpdated` | ported | emitted synchronously by `set_goal` |
| `event('path_stop')` | pathfinder | `BotEvent::PathStop` | ported | `stop()`, a follow goal going invalid, or azalea clearing the goal without reaching (world change, internal stop) |
| `event('path_reset')` | pathfinder | `BotEvent::PathReset{reason}` | diverges | only `goal_updated`/`movements_updated` reasons are observable; azalea's internal repaths (stuck, block change, chunk load, dig error, …) surface as `path_update` and can't be told apart |
| `collectBlock.collect(block)` | pathfinder | `bot.collect_block().collect(b).await` | pending | U12; find+goto+dig loop |
| `pvp.attack(target) / .stop()` | pathfinder | `bot.pvp().attack(e) / stop()` | pending | U12 |
| `event('stoppedAttacking')` | pathfinder | `BotEvent::StoppedAttacking` | pending | U12 |
| `tool.equipForBlock etc.` | pathfinder | `bot.tool() over azalea auto_tool` | pending | U12 |
| `autoEat.*` | pathfinder | `bot.auto_eat()` | pending | U12 stretch |

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

<!-- 463 rows -->
