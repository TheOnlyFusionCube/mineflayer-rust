# Azalea feature survey

Read-only survey of the Azalea Minecraft bot framework at `ref/azalea` for building a Mineflayer-style facade.

- Azalea version: `0.16.0` (workspace `0.16.0+mc26.2`)
- Minecraft version: `26.2` (single-version framework; multi-version via external `azalea-viaversion` plugin)
- Repo: `https://github.com/azalea-rs/azalea`, Rust edition 2024, nightly-only (`#![feature(...)]` in several crates)
- `azalea` crate cargo features: `default = ["log", "serde", "packet-event", "online-mode"]`; `packet-event` exposes raw inbound packets, `online-mode` enables Microsoft auth/signing

## Architecture model

- Everything is Bevy ECS (`bevy_ecs`, Bevy 0.17). `azalea::Client` is a cloneable handle: `{ pub entity: Entity, pub ecs: Arc<RwLock<World>> }`.
- High-level `Client` methods write ECS messages/events (`ecs.write_message(...)`) or trigger observers; systems do the packet work.
- Public-facing ECS "events" are `bevy_ecs` `Message`/`EntityEvent` types; bots mostly subscribe via the `Event` handler or plugins.
- User code typically: `ClientBuilder::new().set_handler(handle).start(account, addr).await`, or ECS systems added via `.add_plugins(...)` + `app.add_systems(GameTick, ...)`.
- Error type: `azalea::client_impl::error::MissingComponentError` / `azalea::error::AzaleaResult<T>` (`azalea::Result<T>` alias).
- Tick broadcasters: `Client::get_tick_broadcaster()`, `Client::get_update_broadcaster()`, `Client::wait_ticks(n)`, `Client::wait_updates(n)` (`azalea::tick_broadcast`).
- `azalea::TokioRuntimeHandle` resource for spawning tokio tasks inside systems.

## Crate map

- `azalea` — top-level facade: `Client`, `ClientBuilder`, `Event`, `EntityRef`, `Swarm`/`SwarmBuilder`, `pathfinder` (integrated, not a separate crate), `container` handles, `auto_tool`, `nearest_entity`, `bot`, plugins (`BotPlugin`, `PathfinderPlugin`, `AcceptResourcePacksPlugin`, `AutoReconnectPlugin`, `AutoRespawnPlugin`, `TickBroadcastPlugin`), `prelude`; re-exports all other crates (`azalea::auth`, `azalea::block`, `azalea::brigadier`, `azalea::buf`, `azalea::chat`, `azalea::core`, `azalea::entity`, `azalea::physics`, `azalea::protocol`, `azalea::registry`, `azalea::world`, `azalea::app`/`azalea::ecs` for bevy).
- `azalea-auth` — Microsoft/Xbox/Minecraft auth flows (`azalea_auth::auth`, device-code login, token cache `azalea_auth::cache`, `certs` for chat signing, `sessionserver`), `game_profile`.
- `azalea-block` — generated block registry + `BlockState`, `BlockTrait`, `BlockBehavior` (destroy_time, friction, etc.), `BlockStates`, `fluid_state::FluidState`, `blocks`/`properties` (generated, `azalea_block::properties` property setters).
- `azalea-brigadier` — Brigadier command tree (`azalea_brigadier::prelude::{literal, argument, bool, integer, float, double, string, word, greedy_string, get_*}`, `CommandDispatcher`, `CommandContext`).
- `azalea-buf` — `AzBuf` serialization trait + `AzBufVar`, `BufReadError`, `azalea_buf_macros` derives, `BitStorage` helpers; protocol (de)serialization.
- `azalea-chat` — chat component model: `FormattedText`, `style::Style`, `ClickEvent`, `HoverEvent`, `TextComponent`, `TranslatableComponent`, `base_component`; `to_ansi()`, serde support.
- `azalea-client` — client plugins: `account::Account`, `client` (bundles, `start_ecs_runner`, `InGameState`/`InConfigState`), `local_player` (`WorldHolder`, `TabList`, `Hunger`, `Experience`, `PermissionLevel`), `player::PlayerInfo`, `ping` (server-list ping), `plugins::*` (packet handlers, movement, mining, interact, inventory, chat, join/disconnect, chunks, etc.).
- `azalea-core` — math/data types: `position::{Vec3, BlockPos, ChunkPos, ChunkSectionPos...}`, `aabb`, `hit_result::{HitResult, BlockHitResult, EntityHitResult}`, `direction`, `game_type::GameMode`, `difficulty`, `entity_id::MinecraftEntityId`, `tick::GameTick`/`Tick` schedules, `heightmap_kind`, `tier`, `objectives`, `sound`, `math`, `bitset`, `delta`, `cursor3d`, `filterable`.
- `azalea-crypto` — AES-128 CFB8 encryption helpers, `offline::generate_uuid`, `signing` (chat message signatures), `digest_data`/`hex_digest`.
- `azalea-entity` — all ECS components for entities: `Position`, `LookDirection`, `Physics`, `PlayerAbilities`, `FluidOnEyes`, `Jumping`, `EntityUuid`, `LocalEntity`, `Dead`, `InLoadedChunk`, `LastSentPosition`, `attributes::{Attributes, AttributeInstance}`, `dimensions::EntityDimensions`, `effects::{ActiveEffects, MobEffectData}`, `inventory::Inventory`, `metadata::*` (generated per-entity marker/metadata components), `mining::get_mine_progress`, `particle`, `plugin::*` (`EntityPlugin`, indexing `EntityUuidIndex`/`EntityIdIndex`, `components`, bundles).
- `azalea-inventory` — item stack & menu model: `ItemStack`/`ItemStackData`, `DataComponentPatch`, `components::*` (item data components incl. `MaxStackSize`), `default_components`, `item::ItemTrait`, `operations::{ClickOperation, PickupClick, QuickMoveClick, SwapClick, CloneClick, ThrowClick, QuickCraftClick, PickupAllClick, ClickType}`, generated `Menu` enum + `Player` inventory struct, `MenuKind` mapping.
- `azalea-language` — static `en_us.json` lookup: `azalea_language::get(key) -> Option<&str>`.
- `azalea-physics` — vanilla-accurate movement: `PhysicsPlugin`, `PhysicsSystems`, `client_movement::{ClientMovementState, WalkDirection, SprintDirection, DirectionStates}`, collision (`collision::{move_colliding, clip}`), `fluids`, `travel`, `climbable`.
- `azalea-protocol` — all packet defs: `packets::{game::{c_*, s_*}, status, login, handshake, configuration}`, `ClientboundGamePacket`/`ServerboundGamePacket` enums, `common::{ClientInformation, client_information::*}`, `connect::{Connection, RawConnection, Proxy, ConnectionError}`, `resolve::{ResolvableAddr, ResolvedAddr, ResolveError}` (SRV-aware), `address::ServerAddr`, `read`/`write` codecs, re-exports `simdnbt`.
- `azalea-registry` — static registries: `builtin::{BlockKind, ItemKind, EntityKind, MobEffect, Attribute, Biome, MenuKind, SoundEvent, ParticleKind, ...}`, `data::*` (Enchantment, DamageKind, Biome, variants), `identifier::Identifier`, `tags`, `ResolvableDataRegistry` + `RegistryHolder` for dynamic registries.
- `azalea-world` — chunk storage & world: `World` (with `entities_by_chunk`, `entity_by_id`, `registries`), `PartialWorld`, `container::{Worlds, WorldName}` resource, `chunk::{Chunk, Section, storage::ChunkStorage, partial::PartialChunkStorage}`, `find_blocks::{find_block, find_blocks, FindBlocks}`, `heightmap`, `iterators`, `palette`/`bit_storage`.
- `azalea-protocol/fuzz` — fuzz target crate for packet decoding; not public API.

## Main `azalea` facade API

- `azalea::ClientBuilder` (builder.rs) — wraps `SwarmBuilder`; `ClientBuilder::new()` adds `azalea_client::DefaultPlugins` + `DefaultBotPlugins`; `new_without_plugins()`, `set_handler(fn(Client, Event, S) -> Fut)` (types `azalea::HandleFn<S, Fut>` / `BoxHandleFn<S, R>`), `set_state(S)`, `add_plugins(...)`, `reconnect_after(Option<Duration>)` (default 5s auto-reconnect), `start(account, address).await -> AppExit`, `start_with_opts(account, address, JoinOpts).await -> AppExit`.
- `azalea::prelude` — `Account`, `Client`, `ClientBuilder`, `Event`, `GameTick`, `AppExit`, `PathfinderClientExt`, `Component`, `Resource`, `azalea::ecs as bevy_ecs`.
- `azalea::JoinOpts` — `server_proxy`, `sessionserver_proxy` (SOCKS5 `azalea_protocol::connect::Proxy`), `custom_server_addr: ServerAddr`, `custom_socket_addr: SocketAddr`; builder methods `proxy()`, `server_proxy()`, `sessionserver_proxy()`, `custom_server_addr()`, `custom_socket_addr()`.
- `azalea::NoState` — marker for stateless bots.
- `azalea::StartClientOpts` — `new(account, ResolvedAddr, Option<UnboundedSender<Event>>)`, `.proxy(Proxy)`; used by `Client::start_client`.
- `azalea::EntityRef` — `{ client, entity }`; methods `id()`, `kind() -> EntityKind`, `uuid()`, `component::<T>()`, `get_component::<T>()`, `query_self::<D,R>()`, `try_query_self`, `attack()`, `interact()`, `look_at()`, `distance_to_client()`; shares `position()`, `dimensions()`, `eye_position()`, `health()`, `minecraft_id()`, `attributes()`, `world_name()`, `is_alive()`, `exists()`, `physics()` with `Client` via a macro (`entity_ref/shared_impls.rs`).
- `azalea::bot` — `Bot` marker component, `BotPlugin`, `JumpEvent`, `LookAtEvent`; `Client::jump()`, `Client::look_at(Vec3)`, `Client::mine(BlockPos) async`, `Client::wait_ticks(n)`, `Client::wait_updates(n)`.

## Connection & auth

- `azalea::ClientBuilder::start(account, impl ResolvableAddr)` / `Client::join(account, addr) -> (Client, UnboundedReceiver<Event>)`, `Client::join_with_proxy(account, addr, Proxy)`.
- `azalea_client::account::Account` — `Arc<dyn AccountTrait>` + Component; `Account::offline(username) -> Account` (account/offline.rs), `Account::microsoft(email).await -> Result<Account, AuthError>` (device-code + cache file `~/.minecraft/azalea-auth-cache.json`), `Account::microsoft_with_opts(MicrosoftAccountOpts)` (custom cache file/client id), `Account::with_microsoft_access_token(token)`, `microsoft_with_custom_client_id_and_scope`, `with_microsoft_access_token_and_custom_client_id_and_scope`.
- `azalea_client::account::AccountTrait` — `username()`, `uuid()`, `access_token()`, `refresh()`, `certs()`/`set_certs()`, `join()` (sessionserver); custom auth possible via `impl AccountTrait` → `.into()`.
- `azalea_auth::auth` — `auth(cache_key, AuthOpts)`, `get_ms_link_code`, `get_ms_auth_token`, `interactive_get_ms_auth_token`, `refresh_ms_auth_token`, `get_minecraft_token`, `check_ownership`, `get_profile`; `azalea_auth::cache::{get_account_in_cache, set_account_in_cache, remove_account_in_cache}`; `azalea_auth::certs::fetch_certificates` (chat signing keys).
- `azalea_protocol::address::{ResolvableAddr, ServerAddr, ResolvedAddr, ServerAddrParseError}` — `ResolvableAddr` impl'd for `&str`, `String`, `ServerAddr`, `(host, port)`...; `azalea_protocol::resolve` — DNS/SRV resolution, `ResolveError`, fallback to Google DNS.
- `Client::disconnect()`, `Client::exit()` (writes `AppExit`), `Swarm::exit()`.
- `azalea_client::ping::ping_server(addr) -> Result<ClientboundStatusResponse, PingError>` + `ping_server_with_proxy`, `ping_server_with_connection` — server-list ping/MOTD.
- `azalea_client::connection` — `Connection`, `RawConnection` components; `Client::with_raw_connection` / `with_raw_connection_mut`.
- ECS events: `azalea::join::{StartJoinServerEvent, ConnectOpts, ConnectionFailedEvent}`, `azalea::disconnect::{DisconnectEvent, IsConnectionAlive}`, `azalea::login::LoginEvent` (internal), `Event::ConnectionFailed(Arc<ConnectionError>)`, `Event::Disconnect(Option<FormattedText>)`.
- `azalea::auto_reconnect::{AutoReconnectPlugin, AutoReconnectDelay}` — default 5s rejoin in `ClientBuilder`/`SwarmBuilder::reconnect_after`.
- `azalea_client::local_player::{InConfigState? (client::InConfigState, InGameState)}` state components; `azalea_client::packet::config` for config-state packets.
- Address in handshake/connection overridable per bot via `JoinOpts::custom_server_addr` / `custom_socket_addr`.

## Chat (send/receive)

- `azalea::Client::chat(content)` — sends `SendChatEvent`; strings starting with `/` auto-become signed command packets (see `client_chat::ChatKind::{Message, Command}`).
- `azalea::Client::write_chat_packet(&str)` — force chat message; `Client::write_command_packet(&str)` — force command (without leading `/`).
- `azalea::client_chat::ChatPacket` — enum `System(ClientboundSystemChat) | Player(ClientboundPlayerChat) | Disguised(ClientboundDisguisedChat)`; methods `message() -> FormattedText`, `split_sender_and_content() -> (Option<String>, String)`, `sender()`, `sender_uuid()`, `content()`, `is_whisper()`, `ChatPacket::new(&str)`.
- `Event::Chat(ChatPacket)` to user handler; ECS events `client_chat::ChatReceivedEvent { entity, packet }`, `SendChatEvent`, `SendChatKindEvent`.
- `azalea_client::chat_signing` (online-mode only) — signs outgoing chat with certs from `azalea_auth::certs`; `ChatSigningPlugin`.
- `azalea_chat` crate: `FormattedText` (+ `to_ansi()`, `to_string()`, `to_html`?), `style`, `click_event`, `hover_event`, `text_component`, `translatable_component`, `base_component`, `numbers` (feature).
- Whisper handling is purely by parsing (`is_whisper` checks for whisper-style components); no dedicated whisper API beyond sending `/msg` via `chat()`.

## World access (chunks / blocks / biomes)

- `Client::world() -> AzaleaResult<Arc<RwLock<azalea_world::World>>>` — shared world (superset in swarms); `Client::partial_world() -> Arc<RwLock<PartialWorld>>` — this client's loaded slice.
- `azalea_world::World::get_block_state(BlockPos) -> Option<BlockState>`, `get_fluid_state`, `get_biome`, `set_block_state` (client-side prediction helper).
- `World::find_block(nearest_to, &BlockStates) -> Option<BlockPos>` — nearest match (spiral chunk scan); `World::find_blocks(nearest_to, &BlockStates) -> FindBlocks` iterator.
- `azalea_block::BlockStates` — set of block states; `From<&BlockKind>`/`From<BlockState>`; `BlockState`, `BlockTrait::{behavior, id, as_block_state, as_block_kind, set_property, ...}`, `BlockBehavior` (destroy_time, friction, jump/speed factor, collision shape...), `azalea_block::fluid_state::FluidState`.
- `azalea_world::chunk::{Chunk { sections: Box<[Section]>, heightmaps: HashMap<HeightmapKind, Heightmap> }, Section { block_count, fluid_count, states, biomes }, storage::ChunkStorage { get, get_block_state, set_block_state, limited_get, min_y, height, ... }, partial::PartialChunkStorage }`; `ChunkStorageTrait` allows custom storage impls.
- `azalea::chunks::{ChunksPlugin, ReceiveChunkEvent { entity, pos, chunk } , ChunkBatchInfo, ChunkBatchStartEvent, ChunkBatchFinishedEvent, ChunkReceiveSpeedAccumulator}`; `Event::ReceiveChunk(ChunkPos)`.
- `azalea_client::local_player::WorldHolder { partial, shared }` component; `azalea_world::{Worlds, WorldName}` ECS resource/component; `World::entities_by_chunk`, `entity_by_id` maps for entity-in-world tracking; `World::registries: RegistryHolder` (dimension-baked registries).
- `Event::Spawn` when fully in a loaded chunk; `azalea::packet::game::WorldLoadedEvent` ECS message on world/dimension load; `loading::PlayerLoadedPlugin`.
- No block-entity (NBT) storage: `ClientboundBlockEntityData` handler is a no-op; `Chunk` has no block-entity map → no sign text, chest contents, spawner data, etc. via world API.

## Entities (query / tracking / metadata)

- `Client::entity() -> EntityRef` (self), `Client::entity_ref_for(Entity)`, `EntityRef` methods (above).
- Lookups: `Client::entity_by_uuid(Uuid)`, `entity_by_minecraft_id(MinecraftEntityId)`, `entity_id_by_uuid`, `entity_id_by_minecraft_id`, `player_uuid_by_username`, `nearest_entity_by::<Q,F>(predicate)`, `nearest_entity_id_by`, `nearest_entities_by`, `nearest_entities::<F>()`, `any_entity_by::<Q,F>(f)`, `any_entity_id_by`, `nearby_players()`.
- `azalea::nearest_entity::EntityFinder<F>` — Bevy `SystemParam` for systems: `nearest_to_position`, `nearest_to_entity`, `nearby_entities_to_position`, `nearby_entities_to_entity`.
- Component reads: `Client::component::<T>() -> AzaleaResult<MappedRwLockReadGuard<T>>`, `get_component`, `query_self::<D,R>(f)`, `try_query_self`, `query_entity::<D,R>(entity, f)`, `try_query_entity`, `entity_component::<T>(e)`, `get_entity_component::<T>(e)`.
- `azalea_entity::metadata::*` — generated components per entity type and data field, e.g. `metadata::Player`, `metadata::AbstractMonster`, `metadata::ItemItem(pub ItemStack)` (item drop), `Health`, `CustomName`, `Sprinting`, `Swimming`, `Invisible`, `Pose`, `AirSupply`, `OnFire`, `Pose`, `SleepingPos`, `metadata::<Kind>` marker structs + `*MetadataBundle` bundles; each data value inserted as its own component on the entity.
- `azalea_entity::plugin` — `EntityPlugin` adds/removes/updates entity tracking; `components::{LocalEntity, Dead, InLoadedChunk, EntityUuid, Jumping, LastSentPosition, LoadedBy?}`, `indexing::{EntityUuidIndex, EntityIdIndex}` world-level maps.
- `azalea_entity::attributes::{Attributes, AttributeInstance, AttributeModifier}` (+ modifier helpers `sprinting_modifier`, `base_attack_speed_modifier`, creative reach modifiers); `Client::attributes()` via shared impl.
- `azalea_entity::effects::{ActiveEffects(HashMap<MobEffect, MobEffectData>), MobEffectData { amplifier, duration, flags }, get_level(), get_dig_speed_amplifier()}` — handled from `c_update_mob_effect`/`c_remove_mob_effect`.
- `azalea_entity::dimensions::EntityDimensions` (`width, height, eye_height, ...`), `azalea_entity::particle` — particle metadata payloads.
- Player identity: `azalea::player::{PlayerInfo, GameProfileComponent(GameProfile)}` — `GameProfile { uuid, name, properties }`; `GameMode` component (`azalea_core::game_type::GameMode`), `PreviousGameMode`, `PermissionLevel`, `PlayerAbilities` (`invulnerable, flying, may_fly, instabuild, fly_speed, walk_speed`), `azalea_entity::inventory::Inventory`.
- Events: `Event::{AddPlayer, RemovePlayer, UpdatePlayer}(PlayerInfo)`, `Event::Death(Option<Arc<ClientboundPlayerCombatKill>>)`; ECS `packet::game::{AddPlayerEvent, RemovePlayerEvent, UpdatePlayerEvent, DeathEvent, KeepAliveEvent, GamePingEvent}`; `interact::SwingArmEvent`; `movement::KnockbackEvent`.
- No generic "entity equipment" facade: `c_set_equipment` handler only `debug!`s — armor/held items of other entities not exposed (item stacks inside entity metadata like `ItemItem`/`FireworksItem` are stored, but equipment slots aren't).

## Movement & physics

- `Client::walk(WalkDirection)`, `Client::sprint(SprintDirection)`, `Client::jump()` (one-tick), `Client::set_jumping(bool)`/`jumping()`, `Client::set_crouching(bool)`/`crouching()`, `Client::set_direction(y_rot, x_rot)`, `Client::direction() -> LookDirection`, `Client::look_at(Vec3)`, `Client::movement_state() -> ClientMovementState`.
- `azalea_client::{ClientMovementState, WalkDirection {Forward,None,Backward?...}, SprintDirection, DirectionStates}` (re-exported via `azalea::` and `azalea_client::` root); ECS `movement::{StartWalkEvent, StartSprintEvent}` (re-exported `azalea::movement::...`), systems `send_position`, `send_player_input_packet`, `send_sprinting_if_needed`, `local_player_ai_step`, `update_pose`.
- `azalea_entity::LookDirection::new(y_rot, x_rot)`, `y_rot()`, `x_rot()`, `update`, `clamped`; helpers `move_relative`, `input_vector`, `view_vector`; `azalea_entity::{Physics { on_ground, last_on_ground, delta, ...; set_on_ground, reset_fall_distance, is_in_water, is_in_lava, clear_fire }, Jumping, FluidOnEyes}`.
- `azalea_physics::{PhysicsPlugin, PhysicsSystems}` runs in `GameTick` schedule: `collision::{move_colliding, clip}`, `fluids`, `travel`, `climbable`; handles gravity, water/lava physics, knockback (`movement::KnockbackEvent`), mob-effect speed/jump modifiers (haste, speed, jump boost, conduit power, mining fatigue), attribute modifiers.
- `azalea_client::packet::game::relative_updates` / `QueuedServerBlockUpdates` — pending block updates per position update.
- Known physics gaps (README): entity pushing and sprint swimming not implemented.

## Block operations (mine / place / interact / attack)

- `Client::mine(BlockPos) async` — mines until done or Mining component removed (bot.rs); `Client::start_mining(BlockPos)`, `is_mining()`, `left_click_mine(bool)` (`LeftClickMine` marker).
- `Client::mine_with_auto_tool(BlockPos) async -> AzaleaResult<()>` (auto_tool.rs) — picks best hotbar tool first; `Client::best_tool_in_hotbar_for_block(BlockState) -> BestToolResult`, `auto_tool::best_tool_in_hotbar_for_block`, `accurate_best_tool_in_hotbar_for_block`.
- Mining internals: `azalea::mining::{MiningPlugin, Mining, MineBundle { MineDelay, MineProgress, MineTicks, MineBlockPos, MineItem }, StartMiningBlockEvent, FinishMiningBlockEvent, StopMiningBlockEvent, MineBlockProgressEvent, AttackBlockEvent, MiningQueued}`; `azalea_entity::mining::get_mine_progress(block, held_item, fluid_on_eyes, physics, attributes, active_effects) -> f32` (efficiency/tool/attribute aware).
- `Client::attack(Entity)` + `Client::has_attack_cooldown()`, `attack_cooldown_remaining_ticks()`; `azalea::attack::{AttackPlugin, AttackEvent, StartAttackEvent?}`; attack cooldown respects attack-speed attributes.
- `Client::block_interact(BlockPos)` — right-click block (use-item-on, predicts block changes via `BlockStatePredictionHandler`); `Client::entity_interact(Entity)` (no crosshair requirement); `Client::start_use_item()` → `interact::{StartUseItemEvent, EntityInteractEvent, SwingArmEvent, InteractPlugin}`; `interact::pick::{pick, pick_block, PickOpts}` — vanilla "pick" raytrace used for `hit_result`.
- `Client::hit_result() -> HitResult` (`azalea_core::hit_result::{HitResult::{Block, Entity}, BlockHitResult { block_pos, direction, location, inside, world_border, miss }, EntityHitResult { entity, location }}`) — crosshair raycast, includes entity targeting.
- Placing blocks = `block_interact` against a supporting block face while holding a block item; no dedicated `place_block` API and no client-side placement prediction yet (README: "doesn't predict the block interactions/placement on the client yet").
- `Client::set_selected_hotbar_slot(u8)` / `selected_hotbar_slot()`; `inventory::SetSelectedHotbarSlotEvent`, `LastSentSelectedHotbarSlot`.
- Block updates: `Event` n/a; ECS `block_update::BlockUpdateEvent`? (plugin `block_update` applies `QueuedServerBlockUpdates`), `QueuedServerBlockUpdates` component.

## Pathfinding

- `azalea::pathfinder::PathfinderClientExt` (implemented for `Client`): `goto(goal).await`, `goto_with_opts(goal, opts).await`, `start_goto(goal)`, `start_goto_with_opts(goal, opts)`, `stop_pathfinding()`, `force_stop_pathfinding()`, `wait_until_goto_target_reached().await`, `is_goto_target_reached()`, `is_executing_path()`, `is_calculating_path()`.
- `azalea::pathfinder::PathfinderPlugin` (part of `DefaultBotPlugins`); components `Pathfinder { goal, opts, is_calculating }`, `ExecutingPath { path, queued_path, last_reached_node, ticks_since_last_node_reached, is_path_partial }`; messages `GotoEvent`, `PathFoundEvent`, `StopPathfindingEvent`.
- Goals (`azalea::pathfinder::goals`): `BlockPosGoal`, `XZGoal { x, z }`, `YGoal`, `RadiusGoal { pos: Vec3, radius }` + `RadiusGoal::new`, `ReachBlockPosGoal`, `OrGoal`, `OrGoals`, `AndGoal`, `AndGoals`; `InverseGoal` deprecated. `Goal` trait for custom goals.
- `azalea::pathfinder::PathfinderOpts::new()` builder: `successors_fn(SuccessorsFn)`, `allow_mining(bool)`, `retry_on_no_path(bool)`, `min_timeout(PathfinderTimeout)`, `max_timeout(...)`; `PathfinderTimeout` (ticks/duration).
- Moves: `pathfinder::moves::{default_move, SuccessorsFn, MoveData, MovesCtx, ExecuteCtx (look_at, walk, sprint, jump, sneak, mine, mine_while_at_start, should_mine, get_block_state), basic, parkour, uncommon, should_mine_block_state, IsReachedCtx, default_is_reached}`.
- `pathfinder::execute::simulation::SimulationPathfinderExecutionPlugin` — alternative smoother execution engine; `pathfinder::debug::PathfinderDebugParticles` component for `/particle` debugging; `pathfinder::world::CachedWorld`, `pathfinder::mining::MiningCache`, `pathfinder::custom_state::{CustomPathfinderStateRef, ...}` for custom cost/state, `pathfinder::positions::RelBlockPos`, `pathfinder::costs`, `pathfinder::astar`.
- Mining while pathfinding uses `Menu::Player` contents to pick tools; water-surface traversal supported as of 0.16.

## Inventory & containers

- `Client::menu() -> AzaleaResult<Menu>` — active `azalea_inventory::Menu` (`Player`, `Generic9x1..6`, `Generic3x3`, `Crafting`, `Furnace`, `Enchantment`, `Merchant`, `Anvil`, `Chest`, `ShulkerBox`, `Hopper`, `Lectern`, ... generated by `declare_menus!`).
- `Client::open_container_at(BlockPos).await -> AzaleaResult<Option<ContainerHandle>>` (5s default timeout), `open_container_at_with_timeout_ticks`; `Client::open_inventory() -> AzaleaResult<Option<ContainerHandle>>` (player's own inventory "container"), `Client::get_inventory() -> AzaleaResult<ContainerHandleRef>`, `Client::get_held_item() -> AzaleaResult<ItemStack>`.
- `azalea::container::{ContainerHandle (Drop closes), ContainerHandleRef}`: `close()`, `id()`, `menu() -> Option<Menu>`, `contents() -> Option<Vec<ItemStack>>` (container-side slots only), `slots() -> Option<Vec<ItemStack>>`, `title() -> Option<FormattedText>`, `left_click(slot)`, `right_click(slot)`, `shift_click(slot)`, `click(ClickOperation)`.
- `azalea_inventory::operations`: `ClickOperation::{Pickup(PickupClick::{Left,Right}), QuickMove(QuickMoveClick::Left), Swap(SwapClick{...}), Clone(CloneClick), Throw(ThrowClick::{Q,CtrlQ}), QuickCraft(QuickCraftClick), PickupAll(PickupAllClick)}` → `slot_num()`, `button_num()`, `click_type()` → `ClickType`; `Menu` has `slots()`, `slot()`, `slot_mut()`, `contents()`, `player_slots_range()`, `hotbar_slots_range()`, `location_for_slot()`, `may_pickup()`, `may_place()`, `max_stack_size()`, `Menu::from_kind(MenuKind)`, `quick_move_stack()` (shift-click semantics).
- `azalea_inventory::{ItemStack::{Empty, Present(ItemStackData)}, ItemStackData { kind: ItemKind, count: i32, components: DataComponentPatch }, ItemStack::{new, is_empty, is_present, count, split, kind, as_present, component_patch, get_component::<T: DataComponentTrait>, with_component}}`; `components::*` generated item data components (`components::MaxStackSize`, `Damage`, `Tool`, `AttributeModifiers`, `WritableBookContent`, ...); `default_components::get_default_component`.
- `azalea_entity::inventory::Inventory` component — `inventory_menu`, `id` (window id), `container_menu`, `container_menu_title`, `carried` (cursor stack), `state_id`, quick-craft state, `selected_hotbar_slot`.
- Inventory plugin events (`azalea::inventory::*` via `pub use azalea_inventory::*` + plugin): `MenuOpenedEvent { entity, window_id, menu_type, title }`, `CloseContainerEvent`, `ClientsideCloseContainerEvent`, `ContainerClickEvent`, `SetContainerContentEvent`, `SetSelectedHotbarSlotEvent`; `equipment_effects` module.
- Creative-mode slot setting: packet `s_set_creative_mode_slot` exists (`ServerboundSetCreativeModeSlot`) but no high-level helper; send via `Client::write_packet`. `GameMode` component tracks mode.

## Crafting & recipes

- No public crafting API. `c_update_recipes`/`c_recipe_book_*` handlers are `debug!`/no-op; recipe list isn't stored; no `Menu::Crafting` helpers for placing items into grid (you'd have to `click` the grid slots manually through the `Crafting`/`Player` menu variants).
- Protocol packets exist: `s_place_recipe::ServerboundPlaceRecipe` (auto-place recipe into grid), `s_recipe_book_*`, `c_update_recipes` — usable via `Client::write_packet`/`SendGamePacketEvent` only.
- No recipe book tracking, no ghost recipe handling (`c_place_ghost_recipe` no-op).

## Scoreboard / teams / bossbar / objectives

- Not stored. `c_set_objective`, `c_set_score`, `c_reset_score`, `c_set_display_objective`, `c_set_player_team` (debug-only), `c_boss_event` handlers are no-ops.
- `azalea_core::objectives::ObjectiveCriteria` exists (Integer/Hearts) for packet types only.
- `c_tab_list` (header/footer) handler is empty → no tab header/footer API.
- Only accessible via `Event::Packet`/`ReceiveGamePacketEvent` raw packets (`c_boss_event::ClientboundBossEvent`, `c_set_score`, `c_set_player_team`, `c_tab_list`, ...).

## Particles / sounds

- No public API or stored state: `c_level_particles`, `c_sound`, `c_sound_entity`, `c_stop_sound`, `c_explode` (knockback only), `c_level_event` handlers log-only/no-op.
- `azalea_registry::builtin::{ParticleKind, SoundEvent}` enums + `azalea_entity::particle`, `azalea_core::sound::CustomSound` types exist; `c_level_particles`/`c_sound` packets parseable via `Event::Packet` or `ReceiveGamePacketEvent`.
- No bot.play_sound / bot.on('soundEffectHeard')-style facade without custom packet plumbing.

## Resource packs

- `azalea::accept_resource_packs::AcceptResourcePacksPlugin` — auto-accepts/denies resource pack push; ECS `packet::game::ResourcePackEvent { entity, id, url, hash, required, prompt }` for custom handling; `c_resource_pack_push`/`c_resource_pack_pop`, `s_resource_pack` packets in protocol.
- No automatic downloading/parsing of pack contents (just the status handshake).

## Tablist & player info

- `Client::tab_list() -> AzaleaResult<HashMap<Uuid, PlayerInfo>>` — wraps `local_player::TabList` component (also `TabListResource`).
- `azalea::player::PlayerInfo { profile: GameProfile { uuid, name, properties }, uuid, gamemode, latency, display_name }`; `GameProfileComponent` on player entities.
- `Event::{AddPlayer, UpdatePlayer, RemovePlayer}` + ECS `packet::game::{AddPlayerEvent, UpdatePlayerEvent, RemovePlayerEvent}` driven by `c_player_info_update`/`c_player_info_remove`.
- Header/footer (`c_tab_list`) dropped (see above).

## Misc public APIs relevant to a facade

- State read: `Client::username()`, `server_username()`, `uuid()`, `profile() -> GameProfile`, `account() -> Account`, `hunger() -> Hunger { food, saturation }`, `experience() -> Experience { progress, level, total }`, `health() -> f32`, `position()`, `eye_position()`, `dimensions()`, `minecraft_id()`, `attributes()`, `physics()`, `is_alive()`, `ticks_connected() -> u64`, `logged_in()`.
- `Client::set_client_information(ClientInformation)` — `azalea_protocol::common::client_information::ClientInformation { language, view_distance, chat_visibility, chat_colors, model_customization, main_hand, text_filtering_enabled, allows_listing, particle_status }`; best sent on `Event::Init`.
- `Client::write_packet(impl Packet<ServerboundGamePacket>)` — arbitrary outbound packet; `Client::with_raw_connection(_mut)`.
- `Event::Packet(Arc<ClientboundGamePacket>)` (feature `packet-event`, on by default) + ECS `packet::game::{ReceiveGamePacketEvent, SendGamePacketEvent}` — escape hatch for everything unsupported.
- Dynamic registries: `Client::with_registry_holder`, `resolve_registry_name`, `resolve_registry_key::<R: ResolvableDataRegistry>`, `with_resolved_registry` (enchantments, damage kinds, biomes, wolf variants, etc. via `azalea_registry::data`/`azalea_registry::builtin`).
- `azalea_client::join::ConnectOpts` (proxy/session-server proxy/handshake overrides internal).
- `azalea_brigadier` for bot-side command trees (used by testbot example; `CommandDispatcher` supports `Result` returns).
- `azalea::tick_broadcast::{TickBroadcast, UpdateBroadcast}` resources; `azalea_core::tick::GameTick` schedule for per-tick systems.
- `Client::nearest_entities_by::<(), With<metadata::Player>>(|_| true)` etc. — Bevy query data + filters as the entity-search API.
- Examples shipped: `echo.rs`, `steal.rs` (find chest → goto → open → shift contents), `nearest_entity.rs` (EntityFinder + LookAtEvent), `testbot/` (swarm + brigadier commands: goto/follow/look/walk/sprint/jump/sneak/mine/killaura/tablist/registry dump), `todo/*` are stale/non-building.

## Notable absences

Public high-level APIs a Mineflayer facade would need but Azalea doesn't provide (packet defs exist for several; handlers are no-op or types only):

- Crafting: no recipe registry, no `craft(item)`; `s_place_recipe` exists but nothing stores `c_update_recipes`.
- Villager trading: `c_merchant_offers` handler empty; `s_select_trade` packet exists; `Menu::Merchant` exists but no offer list/trade API.
- Enchanting table: `Menu::Enchantment` + `s_container_button_click` exist, but no enchant-options parsing (`c_container_set_data` writes generic data fields only).
- Anvil rename/repair: `Menu::Anvil` exists; `s_rename_item` packet exists; no facade.
- Book writing/signing: `s_edit_book`, `c_open_book`, `WritableBookContent` component exist; no write/sign API.
- Fishing: no fishing helpers (`bot.fish()` equivalent); bobber metadata exists (`metadata::FishingHook`) but no wait-for-bite logic.
- Entity equipment slots: `c_set_equipment` is `debug!`-only — can't read another entity's armor/held items.
- Block entities: `c_block_entity_data` no-op; no sign text, chest NBT, spawner data — `Chunk` has no block-entity storage.
- World time/weather: `c_set_time`, `c_level_event` ignored → no `bot.time`, `bot.is_raining`.
- Scoreboard/teams: `c_set_objective`/`c_set_score`/`c_reset_score`/`c_set_display_objective` no-ops; `c_set_player_team` only debug-logs → no team colors, nametag visibility, scoreboard.
- Boss bars: `c_boss_event` parsed but dropped.
- Titles/subtitles/action bar: `c_set_title_text`, `c_set_subtitle_text`, `c_set_action_bar_text`, `c_set_titles_animation`, `c_clear_titles` all dropped.
- Sounds/particles: `c_sound`, `c_sound_entity`, `c_stop_sound`, `c_level_particles`, `c_level_event`, `c_explode` (knockback applied, no event) — no named events.
- Tab header/footer: `c_tab_list` dropped.
- Command suggestions / server-declared commands: `c_command_suggestions`, `c_commands` dropped (no tab-complete or command-graph API).
- Dialogs: `c_show_dialog`/`c_clear_dialog`/`c_custom_click_action` debug-only.
- Advancements/stats/maps: `c_update_advancements`, `c_award_stats`, `c_map_item_data`, `c_debug_*`, `c_game_test_highlight_pos` dropped.
- Damage/death-detail events: `c_damage_event`, `c_take_item_entity`, `c_entity_event` (e.g. eating animation, totem) no-ops → no `entityHurt`-style events except `Event::Death` for self.
- Waypoints (new MC locator bar): `c_waypoint` no-op.
- Creative inventory set slot: no helper for `s_set_creative_mode_slot` (raw packet only).
- Item pickup/drop of own inventory: `ThrowClick` operation exists (drop via click ops) but no `bot.toss`/`bot.tossStack` wrapper; `left_click_mine` only attacks block holding left click.
- Digging completion event: `Client::mine` returns on completion but there's no distinct `diggingCompleted`-style failure reason (cancellation is silent).
- `bot.lookAt` for entities exists (`EntityRef::look_at`) but no forced-perspective/camera (`c_set_camera` ignored).
- Vehicle/pig/horse riding control: `s_move_vehicle`, `s_paddle_boat`, `c_set_passengers`/`c_player_position` handled only for physics; no `bot.mount`/`dismount`/`vehicle` API — metadata `Pose`/passengers not exposed as convenience.
- No `bot.consume`/`eat`/`drink` wrapper (approximable via `start_use_item`); no `bot.activateItem`/`deactivateItem` release packet helper (no `s_player_action` release-use public helper found).
- No `bot.sleep`/bed helper (`block_interact` on bed works; no night/timed handling).
- No `bot.transfer`/server-transfer handling (`c_transfer` packet is dropped).
- No `bot.acceptResourcePack` convenience beyond `AcceptResourcePacksPlugin` (accept-or-decline plugin; no content handling).
- No scoring functions like `bot.experience.level` exists (`Client::experience`), `bot.oxygenLevel` exists only via `AirSupply` metadata component — not wrapped.
- Ping/latency query for self exists only via `tab_list()` lookup; no direct `bot.ping` server latency measure in-game (status ping only via `azalea_client::ping`).
- `Event` lacks variants for: `entityUpdate`/`entitySwingArm`-except-SwingArmEvent ECS, `itemDrop` collection, `playerCollect`, `blockUpdate` notifications (queued internally), `particle`, `sound`, `rain`, `time`, `title`, `scoreboard`, `bossBar`, `experience`/`health` change notifications (pull-based components only — facade must diff components or read raw packets).
