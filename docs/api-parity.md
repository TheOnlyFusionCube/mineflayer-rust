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
| `ScoreBoard (class)` | module | `struct ScoreBoard` | pending | see Scoreboard section |
| `BossBar (class)` | module | `struct BossBar` | pending | see Scoreboard section |
| `Particle (class)` | module | `struct Particle` | pending | see Particles section |
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
| `Entity (prismarine-entity)` | module | `azalea::EntityRef / own Entity type` | pending | — |
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
| `bot.chat(message)` | chat | `bot.chat(&str) -> Result<()>` | ported | /-prefixed -> command; long-message splitting is the chat unit's |
| `bot.whisper(user, msg)` | chat | `bot.whisper(&str, &str)` | pending | /tell shortcut |
| `bot.chatAddPattern(...) (dep)` | chat | `deprecated alias` | pending | deprecated upstream; skip or alias |
| `bot.addChatPattern(name, re, opts)` | chat | `bot.add_chat_pattern(name, Regex, opts) -> usize` | pending | — |
| `bot.addChatPatternSet(name, res, opts)` | chat | `bot.add_chat_pattern_set(name, &[Regex], opts) -> usize` | pending | — |
| `bot.removeChatPattern(name\|id)` | chat | `bot.remove_chat_pattern(key)` | pending | — |
| `bot.awaitMessage(...args)` | chat | `bot.await_message(...) -> impl Future` | pending | string/regex args |
| `bot.tabComplete(str, ...)` | chat | `bot.tab_complete(&str, ...) -> Result<Vec<String>>` | pending | c_command_suggestions dropped by azalea; packet bridge |
| `bot.chatPatterns` | chat | `bot.chat_patterns() -> &[ChatPattern]` | pending | — |
| `bot.tablist` | chat | `bot.tablist() -> (header, footer)` | pending | c_tab_list dropped by azalea; packet bridge |
| `event('chat')` | chat | `BotEvent::Chat(azalea::chat::ChatPacket)` | ported | raw packet payload now; pattern-matched events are the chat unit's |
| `event('whisper')` | chat | `BotEvent::Whisper{...}` | pending | same shape as chat |
| `event('actionBar')` | chat | `BotEvent::ActionBar{..}` | pending | packet bridge: c_set_action_bar_text |
| `event('message')` | chat | `BotEvent::Message{..}` | pending | — |
| `event('messagestr')` | chat | `BotEvent::MessageStr{..}` | pending | — |
| `event('unmatchedMessage')` | chat | `BotEvent::UnmatchedMessage{..}` | pending | typed-only upstream, not emitted |
| `event('chat:{name}')` | chat | `BotEvent::ChatPattern{name, matches}` | pending | dynamic per pattern name |

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
| `bot.nearestEntity(match?)` | entity | `bot.nearest_entity(pred) -> Option<EntityRef>` | pending | azalea nearest_entities_by |
| `bot.findPlayer(filter) (undoc)` | entity | `bot.find_player(f) -> Option<Player>` | pending | — |
| `bot.findPlayers(filter)` | entity | `bot.find_players(f) -> Vec<Player>` | pending | alias of findPlayer upstream |
| `bot.attack(entity, swing)` | entity | `bot.attack(e)` | pending | — |
| `bot.swingArm(hand, showHand)` | entity | `bot.swing_arm(hand)` | pending | — |
| `bot.activateEntity(e)` | entity | `bot.activate_entity(e).await` | pending | azalea entity_interact |
| `bot.activateEntityAt(e, pos)` | entity | `bot.activate_entity_at(e, pos).await` | pending | — |
| `bot.useOn(target)` | entity | `bot.use_on(e)` | pending | saddle/shears |
| `bot.mount(e)` | entity | `bot.mount(e)` | pending | no azalea facade; packet-level |
| `bot.dismount()` | entity | `bot.dismount()` | pending | — |
| `bot.moveVehicle(left, fwd)` | entity | `bot.move_vehicle(..)` | pending | s_move_vehicle |
| `bot.getExplosionDamages(e,pos,r,raw)` | entity | `bot.get_explosion_damages(..) -> Option<f32>` | pending | — |
| `bot.elytraFly()` | entity | `bot.elytra_fly().await` | pending | — |
| `bot._playerFromUUID(uuid)` | entity | `pub(crate) helper` | pending | internal upstream |
| `bot.entity` | entity | `bot.entity() -> azalea::EntityRef` | ported | azalea Client::entity() |
| `bot.entities` | entity | `bot.entities() -> map` | pending | EntityUuidIndex/EntityIdIndex |
| `bot.player` | entity | `bot.player() -> Player` | pending | — |
| `bot.players` | entity | `bot.players() -> HashMap<String, PlayerInfo>` | ported | tab list, keyed by username (upstream) not UUID (azalea) |
| `bot.uuidToUsername (undoc)` | entity | `bot.uuid_to_username()` | pending | — |
| `bot.vehicle (undoc)` | entity | `bot.vehicle() -> Option<EntityRef>` | pending | — |
| `bot.fireworkRocketDuration` | entity | `bot.firework_rocket_duration() -> u32` | pending | — |
| `Entity.skinData` | entity | `entity skin_data() -> Option<SkinData>` | pending | — |
| `Entity.vehicle` | entity | `entity.vehicle()` | pending | — |
| `Entity.position` | entity | `entity.position() -> Vec3` | pending | azalea Position component |
| `Entity.velocity` | entity | `entity.velocity()` | pending | — |
| `Entity.yaw / pitch` | entity | `entity.look_direction()` | pending | — |
| `Entity.onGround` | entity | `entity.on_ground()` | pending | — |
| `Entity.height` | entity | `entity.dimensions().height` | pending | — |
| `Entity.eyeHeight` | entity | `entity.dimensions().eye_height` | pending | — |
| `Player.username` | entity | `player.username()` | pending | — |
| `Player.displayName` | entity | `player.display_name() -> FormattedText` | pending | — |
| `Player.gamemode` | entity | `player.game_mode()` | pending | — |
| `Player.ping` | entity | `player.latency_ms()` | pending | — |
| `Player.entity` | entity | `player.entity() -> Option<EntityRef>` | pending | — |
| `Player.uuid` | entity | `player.uuid()` | pending | — |
| `Player.skinData` | entity | `player.skin_data()` | pending | — |
| `Player.profileKeys` | entity | `player.profile_keys()` | pending | publicKey+signature |
| `event('entitySpawn')` | entity | `BotEvent::EntitySpawn(entity)` | pending | — |
| `event('entityGone')` | entity | `BotEvent::EntityGone(entity)` | pending | — |
| `event('entityMoved')` | entity | `BotEvent::EntityMoved(entity)` | pending | — |
| `event('entityUpdate')` | entity | `BotEvent::EntityUpdate(entity)` | pending | metadata update |
| `event('entityAttributes')` | entity | `BotEvent::EntityAttributes(entity)` | pending | — |
| `event('entitySwingArm')` | entity | `BotEvent::EntitySwingArm(entity)` | pending | azalea SwingArmEvent |
| `event('entityHurt')` | entity | `BotEvent::EntityHurt{entity,source}` | pending | packet bridge; azalea lacks it |
| `event('entityDead')` | entity | `BotEvent::EntityDead(entity)` | pending | — |
| `event('entityTaming')` | entity | `BotEvent::EntityTaming(entity)` | pending | c_entity_event |
| `event('entityTamed')` | entity | `BotEvent::EntityTamed(entity)` | pending | — |
| `event('entityShakingOffWater')` | entity | `BotEvent::EntityShakeWater(entity)` | pending | — |
| `event('entityEatingGrass')` | entity | `BotEvent::EntityEatGrass(entity)` | pending | — |
| `event('entityHandSwap')` | entity | `BotEvent::EntityHandSwap(entity)` | pending | — |
| `event('entityWake')` | entity | `BotEvent::EntityWake(entity)` | pending | — |
| `event('entityEat')` | entity | `BotEvent::EntityEat(entity)` | pending | — |
| `event('entityCriticalEffect')` | entity | `BotEvent::EntityCritical(entity)` | pending | — |
| `event('entityMagicCriticalEffect')` | entity | `BotEvent::EntityMagicCritical(entity)` | pending | — |
| `event('entityCrouch')` | entity | `BotEvent::EntityCrouch(entity)` | pending | metadata pose diff |
| `event('entityUncrouch')` | entity | `BotEvent::EntityUncrouch(entity)` | pending | — |
| `event('entityEquip')` | entity | `BotEvent::EntityEquip(entity)` | pending | c_set_equipment dropped by azalea; bridge |
| `event('entitySleep')` | entity | `BotEvent::EntitySleep(entity)` | pending | — |
| `event('entityElytraFlew')` | entity | `BotEvent::EntityElytraFlew(entity)` | pending | — |
| `event('entityAttach')` | entity | `BotEvent::EntityAttach{entity,vehicle}` | pending | c_set_passengers |
| `event('entityDetach')` | entity | `BotEvent::EntityDetach{entity,vehicle}` | pending | — |
| `event('entityEffect')` | entity | `BotEvent::EntityEffect{entity,effect}` | pending | c_update_mob_effect |
| `event('entityEffectEnd')` | entity | `BotEvent::EntityEffectEnd{entity,effect}` | pending | — |
| `event('entityPlaced') (undoc)` | entity | `BotEvent::EntityPlaced(entity)` | pending | emitted by placeEntity |
| `event('itemDrop')` | entity | `BotEvent::ItemDrop(entity)` | pending | — |
| `event('playerCollect')` | entity | `BotEvent::PlayerCollect{collector,collected}` | pending | c_take_item_entity |
| `event('playerJoined')` | entity | `BotEvent::PlayerJoined(PlayerInfo)` | ported | azalea Event::AddPlayer |
| `event('playerUpdated')` | entity | `BotEvent::PlayerUpdated(PlayerInfo)` | ported | azalea Event::UpdatePlayer |
| `event('playerLeft')` | entity | `BotEvent::PlayerLeft(PlayerInfo)` | ported | azalea Event::RemovePlayer |
| `event('usedFirework')` | entity | `BotEvent::UsedFirework(entity_id)` | pending | — |

## Movement / physics / controls

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.setControlState(c, state)` | movement | `bot.set_control_state(Control, bool)` | pending | — |
| `bot.getControlState(c)` | movement | `bot.control_state(Control) -> bool` | pending | — |
| `bot.clearControlStates()` | movement | `bot.clear_control_states()` | pending | — |
| `bot.look(yaw, pitch, force?)` | movement | `bot.look(yaw, pitch).await` | pending | yaw CCW rad from east |
| `bot.lookAt(point, force?)` | movement | `bot.look_at(vec3).await` | pending | azalea Client::look_at |
| `bot.waitForTicks(n)` | movement | `bot.wait_for_ticks(n) -> Result<()>` | ported | azalea wait_ticks + upstream's n*50ms+5s timeout |
| `bot._replyOnNextTick(reply)` | movement | `pub(crate) helper` | pending | internal upstream |
| `bot.controlState` | movement | `bot.control_states() -> ControlState` | pending | forward/back/left/right/jump/sprint/sneak |
| `bot.physics` | movement | `bot.physics() -> PhysicsSettings` | pending | editable prismarine-physics knobs -> azalea physics where exposed |
| `bot.physicsEnabled` | movement | `bot.physics_enabled() -> bool` | pending | — |
| `bot.jumpQueued (undoc)` | movement | `bot.jump_queued() -> bool` | pending | — |
| `bot.jumpTicks (undoc)` | movement | `bot.jump_ticks() -> u32` | pending | — |
| `bot.abilities` | movement | `bot.abilities() -> Abilities` | pending | PlayerAbilities component |
| `event('move')` | movement | `BotEvent::Move{old_pos}` | pending | — |
| `event('forcedMove')` | movement | `BotEvent::ForcedMove` | pending | server teleport/spawn |
| `event('mount')` | movement | `BotEvent::Mounted{vehicle}` | pending | — |
| `event('dismount')` | movement | `BotEvent::Dismounted{vehicle}` | pending | — |
| `event('physicsTick')` | movement | `BotEvent::Tick` | ported | azalea Event::Tick |
| `event('physicTick') (dep)` | movement | `alias of physicsTick` | ported | deprecated; same BotEvent::Tick |
| `event('abilities')` | movement | `BotEvent::Abilities(abilities)` | pending | — |

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
| `bot.scoreboards` | misc | `bot.scoreboards() -> &Map<name,ScoreBoard>` | pending | reset each login; packet bridge |
| `bot.scoreboard` | misc | `bot.scoreboard() -> &Map<slot,ScoreBoard>` | pending | display slots incl. aliases |
| `bot.teams` | misc | `bot.teams() -> &Map<name,Team>` | pending | — |
| `bot.teamMap` | misc | `bot.team_map() -> &Map<member,Team>` | pending | — |
| `bot.bossBars (undoc)` | misc | `bot.boss_bars() -> &[BossBar]` | pending | — |
| `ScoreBoard.name` | misc | `scoreboard.name()` | pending | — |
| `ScoreBoard.title` | misc | `scoreboard.title()` | pending | — |
| `ScoreBoard.itemsMap` | misc | `scoreboard.items_map()` | pending | — |
| `ScoreBoard.items` | misc | `scoreboard.items() -> sorted` | pending | — |
| `ScoreBoard.setTitle(t)` | misc | `scoreboard.set_title(t)` | pending | client-side view mutation |
| `ScoreBoard.add(name,value)` | misc | `scoreboard.add(..)` | pending | — |
| `ScoreBoard.remove(name)` | misc | `scoreboard.remove(..)` | pending | — |
| `Team fields` | misc | `team.{name,friendly_fire,nametag_visibility,collision_rule,color,prefix,suffix,members}` | pending | c_set_player_team data |
| `Team.parseMessage` | misc | `team.parse_message(..)` | pending | — |
| `Team.add / remove / update` | misc | `team.add/remove/update(..)` | pending | — |
| `Team.displayName(member)` | misc | `team.display_name(member) -> FormattedText` | pending | — |
| `BossBar fields` | misc | `bossbar.{entity_uuid,title,health,dividers,color,darken_sky,dragon_bar,create_fog,flags}` | pending | c_boss_event |
| `event('scoreboardCreated')` | misc | `BotEvent::ScoreboardCreated(sb)` | pending | — |
| `event('scoreboardDeleted')` | misc | `BotEvent::ScoreboardDeleted(sb)` | pending | — |
| `event('scoreboardTitleChanged')` | misc | `BotEvent::ScoreboardTitleChanged(sb)` | pending | — |
| `event('scoreUpdated')` | misc | `BotEvent::ScoreUpdated{sb,item}` | pending | — |
| `event('scoreRemoved')` | misc | `BotEvent::ScoreRemoved{sb,item}` | pending | — |
| `event('scoreboardPosition')` | misc | `BotEvent::ScoreboardPosition{slot,sb}` | pending | — |
| `event('teamCreated')` | misc | `BotEvent::TeamCreated(team)` | pending | — |
| `event('teamRemoved')` | misc | `BotEvent::TeamRemoved(team)` | pending | — |
| `event('teamUpdated')` | misc | `BotEvent::TeamUpdated(team)` | pending | — |
| `event('teamMemberAdded')` | misc | `BotEvent::TeamMemberAdded(team)` | pending | — |
| `event('teamMemberRemoved')` | misc | `BotEvent::TeamMemberRemoved(team)` | pending | — |
| `event('bossBarCreated')` | misc | `BotEvent::BossBarCreated(bar)` | pending | — |
| `event('bossBarDeleted')` | misc | `BotEvent::BossBarDeleted(bar)` | pending | — |
| `event('bossBarUpdated')` | misc | `BotEvent::BossBarUpdated(bar)` | pending | — |
| `event('title')` | misc | `BotEvent::Title{text,kind}` | pending | — |
| `event('title_times')` | misc | `BotEvent::TitleTimes{fade_in,stay,fade_out}` | pending | — |
| `event('title_clear')` | misc | `BotEvent::TitleClear` | pending | — |

## Particles / sound

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `Particle fields` | misc | `particle.{id,name,position,offset,long_distance_render,count,movement_speed}` | pending | c_level_particles |
| `Particle.fromNetwork(packet)` | misc | `Particle::from_packet(..)` | pending | — |
| `event('particle')` | misc | `BotEvent::ParticleSpawned(particle)` | pending | — |
| `event('soundEffectHeard')` | misc | `BotEvent::SoundHeard{name,pos,vol,pitch}` | pending | — |
| `event('hardcodedSoundEffectHeard')` | misc | `BotEvent::HardcodedSound{id,category,pos,vol,pitch}` | pending | — |
| `event('noteHeard')` | misc | `BotEvent::NoteHeard{block,instrument,pitch}` | pending | — |

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
| `event('health')` | state | `BotEvent::Health` | pending | component diff |
| `event('death')` | state | `BotEvent::Death` | ported | azalea Event::Death (payload packet dropped) |
| `event('breath')` | state | `BotEvent::Breath` | pending | — |
| `event('experience')` | state | `BotEvent::Experience` | pending | — |
| `event('spawnReset')` | state | `BotEvent::SpawnReset` | pending | bed spawn invalidated |

## Bed / sleep

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.sleep(bedBlock)` | misc | `bot.sleep(block).await` | pending | block_interact on bed + timing |
| `bot.wake()` | misc | `bot.wake().await` | pending | — |
| `bot.isABed(bedBlock)` | misc | `bot.is_a_bed(block) -> bool` | pending | — |
| `bot.isSleeping` | misc | `bot.is_sleeping() -> bool` | pending | — |
| `bot.parseBedMetadata (undoc)` | misc | `pub(crate) helper` | pending | — |
| `event('sleep')` | misc | `BotEvent::Sleep` | pending | — |
| `event('wake')` | misc | `BotEvent::Wake` | pending | — |

## Fishing

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.fish()` | misc | `bot.fish().await` | pending | no azalea helper; bobber metadata + particles |

## Settings

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.setSettings(options)` | misc | `bot.set_settings(Settings)` | pending | ClientInformation |
| `bot.settings.chat` | misc | `settings.chat_visibility` | pending | — |
| `bot.settings.colorsEnabled` | misc | `settings.chat_colors` | pending | — |
| `bot.settings.viewDistance` | misc | `settings.view_distance` | pending | — |
| `bot.settings.difficulty` | misc | `settings.difficulty -> u8` | pending | stored setting; not sent in ClientInformation |
| `bot.settings.skinParts` | misc | `settings.model_customization` | pending | — |
| `bot.settings.enableTextFiltering` | misc | `settings.text_filtering_enabled` | pending | — |
| `bot.settings.enableServerListing` | misc | `settings.allows_listing` | pending | — |
| `bot.settings.particleStatus` | misc | `settings.particle_status` | pending | — |
| `bot.settings.mainHand (typed-only)` | misc | `settings.main_hand` | pending | — |

## Resource packs

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.acceptResourcePack()` | misc | `bot.accept_resource_pack()` | pending | AcceptResourcePacksPlugin or manual |
| `bot.denyResourcePack()` | misc | `bot.deny_resource_pack()` | pending | — |
| `event('resourcePack')` | misc | `BotEvent::ResourcePack{url,hash,uuid}` | pending | ResourcePackEvent |

## Creative mode (bot.creative)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `bot.creative.setInventorySlot(slot,item)` | inventory | `bot.creative().set_inventory_slot(..).await` | pending | s_set_creative_mode_slot; raw packet only upstream |
| `bot.creative.clearSlot(slot)` | inventory | `bot.creative().clear_slot(..).await` | pending | — |
| `bot.creative.clearInventory()` | inventory | `bot.creative().clear_inventory().await` | pending | — |
| `bot.creative.flyTo(dest)` | inventory | `bot.creative().fly_to(vec3).await` | pending | — |
| `bot.creative.startFlying()` | inventory | `bot.creative().start_flying()` | pending | — |
| `bot.creative.stopFlying()` | inventory | `bot.creative().stop_flying()` | pending | — |

## Plugin ecosystem (in port scope — U7/U12)

| mineflayer API | domain | Rust signature (planned) | status | notes |
|---|---|---|---|---|
| `pathfinder.goto(goal)` | pathfinder | `bot.pathfinder().goto(goal).await` | pending | mineflayer-pathfinder; azalea PathfinderClientExt |
| `pathfinder.setGoal(goal)` | pathfinder | `pathfinder.set_goal(goal)` | pending | — |
| `pathfinder.stop()` | pathfinder | `pathfinder.stop()` | pending | stop_pathfinding |
| `pathfinder.isMoving` | pathfinder | `pathfinder.is_moving() -> bool` | pending | — |
| `Movements class` | pathfinder | `struct Movements -> PathfinderOpts` | pending | — |
| `GoalNear` | pathfinder | `RadiusGoal::new(pos, r)` | pending | — |
| `GoalBlock` | pathfinder | `ReachBlockPosGoal / BlockPosGoal` | pending | — |
| `GoalXZ` | pathfinder | `XZGoal` | pending | — |
| `GoalY` | pathfinder | `YGoal` | pending | — |
| `GoalInvert` | pathfinder | `wrapper goal (azalea InverseGoal deprecated)` | pending | custom Goal impl |
| `GoalFollow` | pathfinder | `custom Goal following entity` | pending | — |
| `GoalCompositeAny` | pathfinder | `OrGoal(s)` | pending | — |
| `GoalCompositeAll` | pathfinder | `AndGoal(s)` | pending | — |
| `event('goal_reached')` | pathfinder | `BotEvent::GoalReached` | pending | — |
| `event('path_update')` | pathfinder | `BotEvent::PathUpdate` | pending | — |
| `event('goal_updated')` | pathfinder | `BotEvent::GoalUpdated` | pending | — |
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
