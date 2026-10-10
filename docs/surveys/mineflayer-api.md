# Mineflayer API Survey — v4.39.0

Complete inventory of the public API surface of `mineflayer` (JS), for the Rust port.
Sources: `docs/api.md` (authoritative), `index.d.ts`, `lib/loader.js`, `lib/plugins/*.js`, `docs/README.md`, `docs/unstable_api.md`, `examples/`.

Legend: `(dep)` = deprecated, `(undoc)` = implemented in lib but not in api.md, `(typed-only)` = in index.d.ts but not found in lib source, `(internal)` = `bot._*` unstable internals.
All "Promise" returns are `Promise<T>` as documented. All `bot.on(...)` events listed per domain.

## Module exports (`require('mineflayer')`)

- `createBot(options) -> Bot`
- `Location` — class: `{floored, blockPoint, chunkCorner, blockIndex, biomeBlockIndex, chunkYIndex}`, `new Location(absoluteVector: Vec3)`
- `Painting` — class: `{id, position, name, direction}`
- `ScoreBoard`, `BossBar`, `Particle` — classes (see domains below)
- `testedVersions: string[]` — ['1.8.8' … '26.1']
- `latestSupportedVersion: string`
- `oldestSupportedVersion: string`
- `supportFeature(feature: string, version: string) -> boolean`
- Data enums via `require('minecraft-data')(bot.version)` / `bot.registry`: `mcdata.blocks`, `mcdata.items`, `mcdata.materials`, `mcdata.recipes`, `mcdata.instruments`, `mcdata.biomes`, `mcdata.entities`
- External classes referenced: `Vec3` (vec3), `Entity` (prismarine-entity), `Block` (prismarine-block, plus `block.blockEntity`, `block.getSignText()`), `Biome` (prismarine-biome), `Item` (prismarine-item), `Recipe` (prismarine-recipe), `ChatMessage` (prismarine-chat), `Window` (prismarine-windows)

## Bot lifecycle & core

### createBot options object

- `username` (default 'Player')
- `port` (default 25565)
- `password` (omit → offline mode if no tokens)
- `host` (default 'localhost')
- `version` (default: auto-detect; e.g. "1.12.2")
- `auth` ('mojang' default | 'microsoft' | 'offline')
- `clientToken`, `accessToken` (generated if password given)
- `logErrors` (default true)
- `hideErrors` (default true in api.md; loader.js sets default false)
- `keepAlive` (default true)
- `checkTimeoutInterval` (default 30000 ms)
- `loadInternalPlugins` (default true)
- `storageBuilder` (fn(version, worldName) -> prismarine-provider-anvil-like)
- `client` (pre-built node-minecraft-protocol Client; enables proxying)
- `brand` (client brand string, default 'vanilla')
- `respawn` (auto-respawn, default true; manual via `bot.respawn()`)
- `plugins` — object map `pluginName -> false | true | injectFn`; inject fn signature `(bot, options) => void`
- `physicsEnabled` (default true)
- `chat`, `colorsEnabled`, `viewDistance`, `difficulty`, `skinParts`, `enableTextFiltering`, `enableServerListing`, `particleStatus` — same as `bot.settings.*`
- `chatLengthLimit` (default 100 for <1.11, 256 for >=1.11)
- `defaultChatPatterns` (default true)
- `mainHand` ('left'|'right') (typed-only)
- `maxCatchupTicks` (default 4) (typed-only)
- Passthrough `ClientOptions` from minecraft-protocol: `realms` ({pickRealm}), `profilesFolder`, `session`, `connect`, `agent`, `stream`, etc.

### Lifecycle methods

- `bot.end(reason?) -> void`
- `bot.quit(reason?) -> void` — graceful disconnect, default 'disconnect.quitting'
- `bot.respawn() -> void` — manual respawn when `respawn: false`
- `bot.connect(options: BotOptions) -> void` (typed-only, no lib assignment found)
- `bot.loadPlugin(plugin: fn) -> void`
- `bot.loadPlugins(plugins: fn[]) -> void`
- `bot.hasPlugin(plugin: fn) -> boolean`
- `bot.supportFeature(name: string) -> boolean`
- `bot.tabComplete(str, assumeCommand?, sendBlockInSight?, timeout?) -> Promise<string[]>`

### Lifecycle/core properties

- `bot.username -> string`
- `bot.version -> string` (Minecraft version)
- `bot.protocolVersion -> number|string`
- `bot.majorVersion -> string`
- `bot.registry -> Registry` (prismarine-registry / minecraft-data instance)
- `bot._client -> Client` (unstable API — node-minecraft-protocol client)
- `bot._warn(...msg)` (internal)

### Lifecycle events

- `event('inject_allowed')` — index loaded; can load mcData/plugins; wait for 'spawn' for world ops
- `event('login')` — after successful login
- `event('spawn')` — first spawn and every respawn after death
- `event('respawn')` — dimension change / just before spawn
- `event('connect')` (undoc) — forwarded from client TCP connect
- `event('kicked'): (reason: chatMsg|string, loggedIn: boolean)`
- `event('end'): (reason: string)` — default 'socketClosed'
- `event('error'): (err: Error)`

## Chat / messaging

### Methods

- `bot.chat(message: string) -> void` — splits long messages; throws before 'login'/after early disconnect
- `bot.whisper(username, message) -> void` — "/tell" shortcut
- `bot.chatAddPattern(pattern: RegExp, chatType: string, description?) -> number` (dep — use addChatPattern)
- `bot.addChatPattern(name, pattern: RegExp, opts?{repeat=true, parse, deprecated}) -> number` — alias of addChatPatternSet(name,[pattern])
- `bot.addChatPatternSet(name, patterns: RegExp[], opts?{repeat=true, parse}) -> number`
- `bot.removeChatPattern(name: string|number) -> void`
- `bot.awaitMessage(...args: (string|string[]|RegExp)[]) -> Promise<string>`
- `bot.tabComplete(str, assumeCommand?, sendBlockInSight?, timeout=5000) -> Promise<string[]>`

### Properties

- `bot.chatPatterns -> ChatPattern[]` — `{pattern: RegExp, type: string, description: string}`
- `bot.tablist -> {header: ChatMessage, footer: ChatMessage}`

### Events

- `event('chat'): (username, message, translate, jsonMsg: ChatMessage, matches: string[]|null)`
- `event('whisper'): (username, message, translate, jsonMsg, matches)` — same shape
- `event('actionBar'): (jsonMsg: ChatMessage, verified: null|boolean)`
- `event('message'): (jsonMsg: ChatMessage{unsigned?}, position: 'chat'|'system'|'game_info', sender: UUID|null, verified)`
- `event('messagestr'): (message: string, messagePosition, jsonMsg, sender, verified)` — stringified 'message'
- `event('unmatchedMessage'): (stringMsg, jsonMsg)` (typed-only; declared in index.d.ts, not emitted by current lib)
- `event('chat:{name}'): (matches)` — dynamic event per addChatPattern(Set) name; payload is regex matches (or capture groups if `parse`)

## World / blocks

### Methods

- `bot.blockAt(point: Vec3, extraInfos=true) -> Block|null`
- `bot.waitForChunksToLoad() -> Promise<void>`
- `bot.blockInSight(maxSteps=256, vectorLength=5/16) -> Block|null` (dep — use blockAtCursor)
- `bot.blockAtCursor(maxDistance=256, matcher?) -> Block|null`
- `bot.entityAtCursor(maxDistance=3.5) -> Entity|null`
- `bot.blockAtEntityCursor(entity=bot.entity, maxDistance=256, matcher?) -> Block|null`
- `bot.canSeeBlock(block) -> boolean`
- `bot.findBlocks(options{point?, matching: id|id[]|fn, useExtraInfo?: bool|fn, maxDistance=16, count=1}) -> Vec3[]` (sorted closest-first)
- `bot.findBlock(options) -> Block|null` — `bot.blockAt(bot.findBlocks(options)[0])`
- `bot.updateSign(block, text: string, back=false) -> void`

### Properties

- `bot.world -> world.WorldSync` (prismarine-world sync world; own events below)
- `bot.spawnPoint -> Vec3`

### Events (bot-level; `bot.world` also emits blockUpdate*, prefer that emitter)

- `event('blockUpdate'): (oldBlock: Block|null, newBlock: Block)`
- `event('blockUpdate:(x, y, z)'): (oldBlock|null, newBlock|null)` — per-position; auto-removed on world unload
- `event('blockPlaced'): (oldBlock, newBlock)` — bot's own placements
- `event('blockEntityData'): (block: Block|null)`
- `event('signOpen'): (block: Block|null)` — sign editor opened after placing sign; respond with bot.updateSign
- `event('chunkColumnLoad'): (point: Vec3)` / `event('chunkColumnUnload'): (point: Vec3)`
- `event('pistonMove'): (block, isPulling: number, direction: number)`
- `event('chestLidMove'): (block, isOpen: number, block2: Block|null)`
- `event('blockBreakProgressObserved'): (block, destroyStage: 0-9, entity)`
- `event('blockBreakProgressEnd'): (block, entity)`

## Entities / players / combat

### Methods

- `bot.nearestEntity(match?: (entity)=>boolean) -> Entity|null`
- `bot.findPlayer(filter: string|RegExp|fn|null) -> Entity|Entity[]|null` (undoc) — `bot.findPlayers` is same fn (alias)
- `bot.attack(entity, swing=true) -> void`
- `bot.swingArm(hand?='right'|'left', showHand=true) -> void`
- `bot.activateEntity(entity) -> Promise<void>`
- `bot.activateEntityAt(entity, position: Vec3) -> Promise<void>` — e.g. armor stands
- `bot.useOn(targetEntity) -> void` — saddle, shears
- `bot.mount(entity) -> void` — enter vehicle
- `bot.dismount() -> void`
- `bot.moveVehicle(left: -1|1, forward: -1|1) -> void`
- `bot.getExplosionDamages(entity, position: Vec3, radius: number, rawDamages?) -> number|null`
- `bot.elytraFly() -> Promise<void>` — start elytra flight
- `bot._playerFromUUID(uuid) -> Player` (internal)

### Properties

- `bot.entity -> Entity` — own entity
- `bot.entities -> {[id]: Entity}` — all nearby entities
- `bot.player -> Player {username, displayName: ChatMessage, gamemode, ping, entity|null, uuid, skinData?, profileKeys?{publicKey, signature}}`
- `bot.players -> {[username]: Player}`
- `bot.uuidToUsername -> {[uuid]: username}` (undoc)
- `bot.vehicle -> Entity|null` (undoc) — vehicle bot is riding
- `bot.fireworkRocketDuration -> number` — remaining elytra boost ticks
- `Entity.skinData -> {url, model: 'slim'|'classic'|null, capeUrl?}` (player entities)
- `Entity.vehicle`, `Entity.position`, `Entity.velocity`, `Entity.yaw/pitch`, `Entity.onGround`, `Entity.height`, `Entity.eyeHeight` (from prismarine-entity)

### Events

- `event('entitySpawn'): (entity)` / `event('entityGone'): (entity)` / `event('entityMoved'): (entity)` / `event('entityUpdate'): (entity)` (metadata)
- `event('entityAttributes'): (entity)`
- `event('entitySwingArm'): (entity)`
- `event('entityHurt'): (entity, source?: Entity)`
- `event('entityDead'): (entity)`
- `event('entityTaming'): (entity)` / `event('entityTamed'): (entity)`
- `event('entityShakingOffWater'): (entity)` / `event('entityEatingGrass'): (entity)` / `event('entityHandSwap'): (entity)`
- `event('entityWake'): (entity)` / `event('entityEat'): (entity)`
- `event('entityCriticalEffect'): (entity)` / `event('entityMagicCriticalEffect'): (entity)`
- `event('entityCrouch'): (entity)` / `event('entityUncrouch'): (entity)`
- `event('entityEquip'): (entity)` / `event('entitySleep'): (entity)`
- `event('entityElytraFlew'): (entity)`
- `event('entityAttach'): (entity, vehicle)` / `event('entityDetach'): (entity, vehicle)`
- `event('entityEffect'): (entity, effect:{id, amplifier, duration})` / `event('entityEffectEnd'): (entity, effect)`
- `event('entityPlaced'): (entity)` (undoc) — emitted by placeEntity
- `event('itemDrop'): (entity)`
- `event('playerCollect'): (collector: Entity, collected: Entity)`
- `event('playerJoined'): (player)` / `event('playerUpdated'): (player)` / `event('playerLeft'): (player)`
- `event('usedFirework'): (fireworkEntityId)`

## Movement / physics / control states

### Methods

- `bot.setControlState(control: 'forward'|'back'|'left'|'right'|'jump'|'sprint'|'sneak', state: bool) -> void`
- `bot.getControlState(control) -> boolean`
- `bot.clearControlStates() -> void`
- `bot.look(yaw, pitch, force?) -> Promise<void>` — yaw CCW radians from east; pitch ±π/2
- `bot.lookAt(point: Vec3, force?) -> Promise<void>`
- `bot.waitForTicks(ticks: number) -> Promise<void>` — waits on physics timer
- `bot._replyOnNextTick(reply)` (internal)

### Properties

- `bot.controlState -> {forward, back, left, right, jump, sprint, sneak}` — setters call setControlState
- `bot.physics -> PhysicsOptions {maxGroundSpeed, terminalVelocity, walkingAcceleration, gravity, groundFriction, playerApothem, playerHeight, jumpSpeed, yawSpeed, pitchSpeed, sprintSpeed, maxGroundSpeedSoulSand, maxGroundSpeedWater}` (prismarine-physics instance; editable)
- `bot.physicsEnabled -> boolean`
- `bot.jumpQueued`, `bot.jumpTicks` (undoc — autojump state)
- `bot.abilities -> {invulnerable, flying, mayFly, instantBuild, flyingSpeed, walkingSpeed}` (server-sent abilities)

### Events

- `event('move'): (oldPos?: Vec3)` — bot moved
- `event('forcedMove')` — server teleport/spawn
- `event('mount')` — bot mounted entity (see `bot.vehicle`)
- `event('dismount'): (vehicle: Entity)`
- `event('physicsTick')` — every tick while physicsEnabled
- `event('physicTick')` (dep alias of physicsTick)
- `event('abilities'): (abilities)` — server sent new abilities packet

## Pathfinding

**No pathfinding API exists in core.** All pathing APIs (`bot.pathfinder.*`, `bot.collectBlock.*`, etc.) come from external plugins — see Plugin ecosystem. Core provides only primitives: control states, lookAt, dig, placeBlock, activateBlock, `bot.creative.flyTo`.

## Inventory / containers / windows

### Bot methods — high level

- `bot.equip(item: Item|itemId, destination: 'hand'|'head'|'torso'|'legs'|'feet'|'off-hand'|null) -> Promise<void>`
- `bot.unequip(destination) -> Promise<void>`
- `bot.tossStack(item: Item) -> Promise<void>`
- `bot.toss(itemType, metadata|null, count|null) -> Promise<void>`
- `bot.setQuickBarSlot(slot: 0-8) -> void`
- `bot.getEquipmentDestSlot(destination) -> number` — slot id for 'head'|'torso'|'legs'|'feet'|'hand'|'off-hand'
- `bot.consume() -> Promise<void>` — eat/drink held item
- `bot.activateItem(offHand=false) -> void` — bow, egg, firework, eat start
- `bot.deactivateItem() -> void` — release arrow, stop eating
- `bot.simpleClick.leftMouse(slot) -> Promise<void>` — `clickWindow(slot,0,0)`
- `bot.simpleClick.rightMouse(slot) -> Promise<void>` — `clickWindow(slot,1,0)`

### Bot methods — low level ("prefer high-level")

- `bot.clickWindow(slot, mouseButton, mode) -> Promise<void>` — modes: 0 mouse (stable); 1 shift, 2 number, 3 middle, 4 drop (experimental); 5 drag, 6 double (unimplemented)
- `bot.putSelectedItemRange(start, end, window, slot) -> Promise<void>`
- `bot.putAway(slot) -> Promise<void>`
- `bot.closeWindow(window) -> Promise<void>`
- `bot.transfer(options{window?, itemType, metadata?, sourceStart, sourceEnd?, destStart, destEnd?, count=1, nbt?}) -> Promise<void>`
- `bot.openBlock(block, direction?: Vec3, cursorPos?: Vec3) -> Promise<Window>`
- `bot.openEntity(entity, Class?) -> Promise<Window>`
- `bot.moveSlotItem(sourceSlot, destSlot) -> Promise<void>`
- `bot.updateHeldItem() -> void`

### Bot methods — container openers

- `bot.openContainer(blockOrEntity, direction?, cursorPos?) -> Promise<Container|Chest|Dispenser>`
- `bot.openChest(chestBlockOrMinecartEntity, direction?, cursorPos?) -> Promise<Chest>` (dep — same as openContainer)
- `bot.openFurnace(furnaceBlock) -> Promise<Furnace>`
- `bot.openDispenser(dispenserBlock) -> Promise<Dispenser>` (dep — same as openContainer)
- `bot.openEnchantmentTable(block) -> Promise<EnchantmentTable>`
- `bot.openAnvil(anvilBlock) -> Promise<Anvil>`
- `bot.openVillager(villagerEntity) -> Promise<Villager>` — listen 'ready'
- `bot.trade(villagerInstance, tradeIndex, times?) -> Promise<void>`
- `bot.writeBook(slot, pages: string[]) -> Promise<void>`
- `bot.signBook(slot, pages: string[], author, title) -> Promise<void>` (undoc)
- `bot.setCommandBlock(pos: Vec3, command: string, options{mode: 0|1|2, trackOutput, conditional, alwaysActive}) -> void`

### Properties

- `bot.inventory -> Window` (player inventory window)
- `bot.quickBarSlot -> 0-8`
- `bot.heldItem -> Item|null`
- `bot.usingHeldItem -> boolean` (eating/blocking)
- `bot.currentWindow -> Window|null` (undoc)
- `bot.QUICK_BAR_START -> number` (undoc constant)

### Window classes (extend prismarine-windows `Window`; base: `window.items()`, `window.slots`, `window.deposit(itemType, metadata, count, nbt) -> Promise<void>`, `window.withdraw(itemType, metadata, count, nbt) -> Promise<void>`, `window.close() -> Promise<void>`)

- `Chest`/`Container`/`Dispenser extends Window` — deposit, withdraw, close; events 'open','close','updateSlot(slot, oldItem, newItem)'
- `Furnace extends Window`: `fuel: 0-1`, `progress: 0-1`, `takeInput()/takeFuel()/takeOutput() -> Promise<Item>`, `putInput(itemType, metadata, count)/putFuel(...) -> Promise<void>`, `inputItem()/fuelItem()/outputItem() -> Item`; event 'update'
- `EnchantmentTable extends Window`: `xpseed`, `enchantments: [{level, expected:{enchant, level}} ×3]`, `targetItem() -> Item`, `enchant(choice 0-2) -> Promise<Item>`, `takeTargetItem() -> Promise<Item>`, `putTargetItem(item) -> Promise`, `putLapis(item) -> Promise`; event 'ready'
- `Anvil` (not a Window subclass in d.ts): `combine(itemOne, itemTwo, name?) -> Promise<void>`, `combine(item, name?)`, `rename(item, name?) -> Promise<void>` (undoc in api.md; in d.ts)
- `Villager extends Window`: `trades: VillagerTrade[] {inputItem1, outputItem, inputItem2, hasItem2, tradeDisabled, nbTradeUses, maximumNbTradeUses, xp?, specialPrice?, priceMultiplier?, demand?, realPrice?}`, `selectedTrade`, `trade(index, count?) -> Promise<void>`; event 'ready'

### Events

- `event('windowOpen'): (window)` — workbench/chest/brewing stand opened
- `event('windowClose'): (window)`
- `event('heldItemChanged'): (heldItem: Item|null)`
- Window emitter events: 'open', 'close', 'updateSlot(slot, oldItem, newItem)' (StorageEvents); Furnace adds 'update'; EnchantmentTable/Villager add 'ready'
- Internal transaction events (internal): `confirmTransaction{id}`, `setSlot:{windowId}`, `setWindowItems:{windowId}`

## Crafting / recipes

- `bot.recipesFor(itemType, metadata|null, minResultCount|null, craftingTable: Block|null) -> Recipe[]` — only craftable-with-current-inventory recipes
- `bot.recipesAll(itemType, metadata|null, craftingTable) -> Recipe[]` — ignores inventory contents
- `bot.craft(recipe: Recipe, count?, craftingTable: Block?) -> Promise<void>`
- `Recipe` class from prismarine-recipe; `mcdata.recipes` for raw data

## Block interaction (dig / place / activate)

- `bot.dig(block, forceLook?: bool|'ignore', digFace?: 'auto'|Vec3|'raycast') -> Promise<void>` — resolve on break/interrupt
- `bot.stopDigging() -> void`
- `bot.digTime(block) -> number` (ms)
- `bot.canDigBlock(block) -> boolean` — diggable + in range
- `bot.placeBlock(referenceBlock, faceVector: Vec3) -> Promise<void>` — placed at `referenceBlock.position.plus(faceVector)`; rejects on server refusal
- `bot.placeEntity(referenceBlock, faceVector) -> Promise<Entity>` — armor stand, end crystal etc.
- `bot.activateBlock(block, direction?=Vec3(0,1,0), cursorPos?=Vec3(0.5,0.5,0.5)) -> Promise<void>` — note block, door, etc.
- `bot.activateItem(offHand?)`, `bot.deactivateItem()`, `bot.useOn(entity)` — see Inventory/Entities
- `bot._genericPlace`, `bot._placeBlockWithOptions`, `bot._placeEntityWithOptions` (internal)
- `bot.targetDigBlock -> Block|null`; `bot.targetDigFace`, `bot.lastDigTime` (undoc)
- Events: `diggingCompleted(block)`, `diggingAborted(block)`, `blockPlaced(oldBlock, newBlock)`, `entityPlaced(entity)` (undoc)

## Scoreboard / teams / bossbar / title

### Bot properties

- `bot.scoreboards -> {[name]: ScoreBoard}` — reset each login
- `bot.scoreboard -> {[displaySlot]: ScoreBoard}` — slots: 'belowName','sidebar','list' (aliases of 2,1,0) and 0-18
- `bot.teams -> {[name]: Team}` — reset each login
- `bot.teamMap -> {[memberName|uuid]: Team}` — reset each login
- `bot.bossBars -> BossBar[]` (undoc getter)

### Classes

- `ScoreBoard`: `name`, `title`, `itemsMap {[name]: {name, displayName?, value}}`, `items` (sorted array); methods `setTitle(title)`, `add(name, value) -> item`, `remove(name) -> item`
- `Team`: `team`, `name`, `friendlyFire`, `nameTagVisibility` ('always'|'hideForOtherTeams'|'hideForOwnTeam'), `collisionRule` ('always'|'pushOtherTeams'|'pushOwnTeam'), `color` (e.g. 'dark_green'), `prefix`, `suffix` (ChatMessage), `members[]`, `memberMap`; methods `parseMessage`, `add`, `remove`, `update`, `displayName(member) -> ChatMessage`
- `BossBar`: `entityUUID`, `title` (ChatMessage), `health` 0-1, `dividers` (0|6|10|12|20), `color` ('pink'|'blue'|'red'|'green'|'yellow'|'purple'|'white'), `shouldDarkenSky`, `isDragonBar`, `createFog`/`shouldCreateFog`, `flags`

### Events

- `event('scoreboardCreated'|'scoreboardDeleted'|'scoreboardTitleChanged'): (scoreboard)`
- `event('scoreUpdated'|'scoreRemoved'): (scoreboard, item)`
- `event('scoreboardPosition'): (position, scoreboard)`
- `event('teamCreated'|'teamRemoved'|'teamUpdated'|'teamMemberAdded'|'teamMemberRemoved'): (team)`
- `event('bossBarCreated'|'bossBarDeleted'|'bossBarUpdated'): (bossBar)`
- `event('title'): (title: string, type: 'title'|'subtitle')`
- `event('title_times'): (fadeIn, stay, fadeOut)` — ticks
- `event('title_clear')`

## Particles / sound

- `Particle` class: `id`, `name`, `position: Vec3`, `offset: Vec3`, `longDistanceRender`, `count`, `movementSpeed`; `Particle.fromNetwork(packet)`
- `event('particle'): (particle)` (api.md lists no payload; source emits Particle)
- `event('soundEffectHeard'): (soundName, position: Vec3, volume, pitch)`
- `event('hardcodedSoundEffectHeard'): (soundId, soundCategory, position, volume, pitch)`
- `event('noteHeard'): (block, instrument: {id, name: 'harp'|'doubleBass'|'snareDrum'|'sticks'|'bassDrum'}, pitch: 0-24)`

## Time / weather / game state

### Properties

- `bot.time.doDaylightCycle -> bool`
- `bot.time.bigTime -> BigInt` — ticks since day 0
- `bot.time.time -> number`
- `bot.time.timeOfDay -> number` (mod 24000; 0 sunrise, 6000 noon, 12000 sunset, 18000 midnight)
- `bot.time.day -> number`
- `bot.time.isDay -> bool` (0-13000)
- `bot.time.moonPhase -> 0-7` (0 = full)
- `bot.time.bigAge -> BigInt`, `bot.time.age -> number`
- `bot.isRaining -> bool`, `bot.rainState -> 0-1`, `bot.thunderState -> 0-1`
- `bot.game.levelType -> 'default'|'flat'|'largeBiomes'|'amplified'|'customized'|'buffet'|'default_1_1'`
- `bot.game.dimension -> 'overworld'|'the_nether'|'the_end'`
- `bot.game.difficulty -> 'peaceful'|'easy'|'normal'|'hard'`
- `bot.game.gameMode -> 'survival'|'creative'|'adventure'|'spectator'`
- `bot.game.hardcore -> bool`, `bot.game.maxPlayers`, `bot.game.serverBrand`, `bot.game.minY`, `bot.game.height`

### Events

- `event('time')`, `event('rain')`, `event('weatherUpdate')` (rainState/thunderState changed), `event('game')` (any game.* changed)

## Health / food / experience / oxygen

- `bot.health -> 0-20`, `bot.food -> 0-20`, `bot.foodSaturation -> number`, `bot.oxygenLevel -> 0-20`
- `bot.isAlive -> bool` (undoc)
- `bot.experience.level`, `bot.experience.points`, `bot.experience.progress` (0-1)
- Events: `health`, `death`, `breath`, `experience`, `spawnReset` (bed spawn invalid)

## Bed / sleep

- `bot.sleep(bedBlock) -> Promise<void>`
- `bot.wake() -> Promise<void>`
- `bot.isABed(bedBlock) -> boolean`
- `bot.isSleeping -> boolean`
- `bot.parseBedMetadata` (undoc/internal)
- Events: `sleep`, `wake`

## Fishing

- `bot.fish() -> Promise<void>` — cast rod, resolve when fishing ends (catch/timeout); relies on world_particles + entity_destroy tracking

## Settings

- `bot.setSettings(options: Partial<GameSettings>) -> void`
- `bot.settings.chat -> 'enabled'|'commandsOnly'|'disabled'`
- `bot.settings.colorsEnabled -> bool`
- `bot.settings.viewDistance -> 'far'|'normal'|'short'|'tiny'|number`
- `bot.settings.difficulty -> number`
- `bot.settings.skinParts -> {showCape, showJacket, showLeftSleeve, showRightSleeve, showLeftPants, showRightPants, showHat}` (7 bools)
- `bot.settings.enableTextFiltering -> bool`
- `bot.settings.enableServerListing -> bool`
- `bot.settings.particleStatus -> 'all'|'decreased'|'minimal'` (1.21.3+)
- `bot.settings.mainHand -> 'left'|'right'` (d.ts only)

## Resource packs

- `bot.acceptResourcePack() -> void`
- `bot.denyResourcePack() -> void`
- `event('resourcePack'): (url, hash)` — note source also emits `(uuid, url)` ordering on some versions (d.ts: `(url, hash?, uuid?)`)

## Creative mode (`bot.creative`)

- `bot.creative.setInventorySlot(slot, item: Item|null) -> Promise<void>` — null deletes
- `bot.creative.clearSlot(slot) -> Promise<void>`
- `bot.creative.clearInventory() -> Promise<void>`
- `bot.creative.flyTo(destination: Vec3) -> Promise<void>` — straight-line, no pathfinding, <2 blocks hops intended
- `bot.creative.startFlying() -> void` — gravity := 0
- `bot.creative.stopFlying() -> void`

## Internal plugin list (lib/plugins/*, all loadable via options.plugins name or disabled via `false`)

abilities, anvil, bed, block_actions, blocks, book, boss_bar, breath, chat, chest, command_block, craft, creative, digging, enchantment_table, entities, experience, explosion, fishing, furnace, game, generic_place, health, inventory, kick, particle, physics, place_block, place_entity, rain, ray_trace, resource_pack, scoreboard, sequence, settings, simple_inventory, sound, spawn_point, tablist, team, time, title, villager.

Core deps (prismarine-*): minecraft-protocol (protocol/auth/encryption), minecraft-data, prismarine-{physics, chunk, block, chat, world, windows, item, nbt, recipe, biome, entity, registry}, node-vec3, mojangson, protodef, uuid-1345.

## External plugin ecosystem (from docs/README.md "Third Party Plugins" + tutorial)

| Plugin | Adds |
|---|---|
| mineflayer-pathfinder | A* pathfinding (`bot.pathfinder.goto/setGoal`, Movements, Goal* classes), events 'goal_reached','path_update','goal_updated' |
| mineflayer-collectblock | `bot.collectBlock.collect(block)` — find+navigate+dig block collection API |
| mineflayer-pvp | `bot.pvp.attack(target)`/`.stop()`, 'stoppedAttacking' events — PVP/PVE loop |
| mineflayer-tool | auto tool/weapon selection (`bot.tool.equipForBlock` etc.) |
| mineflayer-auto-eat | `bot.autoEat.*` — auto food eating, 'autoeat_started/stopped' events |
| mineflayer-autocrystal | auto place/break end crystals |
| mineflayer-statemachine | behavior state machine API on top of bot |
| MineflayerArmorManager | auto armor equip management |
| mineflayer-web-inventory | web-based inventory viewer UI |
| mineflayer-dashboard | frontend dashboard for bot |
| minecraftHawkEye | `bot.hawkEye.*` auto-aim for bows/projectiles |
| mineflayer-GUI | nested GUI window interaction with async/await |
| mineflayer-projectile | projectile launch-angle computation |
| mineflayer-movement | smooth realistic player movement (PvP-oriented) |
| mineflayer-radar | web radar via canvas + socket.io |
| MineflayerAutoAuth | chat-based bot authentication |
| mineflayer-bloodhound | damage attribution (who damaged whom) |
| mineflayer-tps | server TPS measurement |
| mineflayer-panorama | panorama screenshots |
| mineflayer-death-event | player-death event emission |
| minecraft-mcp-server | MCP server exposing mineflayer to LLMs |
| prismarine-viewer | browser world viewer (`mineflayerViewer(bot, {port, firstPerson})`) |
| mineflayer-navigate | legacy pathfinding (in tutorial; superseded by pathfinder) |
| mineflayer-builder | schematic printing in survival (projects list) |

## Internal/undocumented bot members (found in lib source)

`bot._client`, `bot._warn`, `bot._nextSequence` (sequence.js), `bot._setSlot`, `bot._syncWindow`, `bot._ensureHasSentCarriedItem`, `bot._updateBlockState`, `bot._getBlockAtEyeLevel`, `bot._getDimensionName`, `bot._placeBlockWithOptions`, `bot._placeEntityWithOptions`, `bot._genericPlace`, `bot._playerFromUUID`, `bot._replyOnNextTick`, `bot.parseBedMetadata`, `bot.vehicle`, `bot.isAlive`, `bot.currentWindow`, `bot.QUICK_BAR_START`, `bot.jumpQueued`, `bot.jumpTicks`, `bot.targetDigFace`, `bot.lastDigTime`, `bot.uuidToUsername`, `bot.bossBars`, `bot.findPlayer`/`bot.findPlayers`, `bot.signBook`.

## Summary counts

Methods counted = bot.* callables incl. namespaced (creative.*, simpleClick.*) and window-class methods; deprecated/undocumented counted; internal `_*` excluded.

| Domain | Methods | Properties | Events |
|---|---|---|---|
| Lifecycle & core | 8 | 6 | 8 |
| Chat / messaging | 8 | 2 | 7 |
| World / blocks | 10 | 2 | 11 |
| Entities / players / combat | 12 | 8 | 32 |
| Movement / physics / controls | 6 | 6 | 7 |
| Pathfinding | 0 | 0 | 0 (all external) |
| Inventory / containers | 11 hi + 10 low + 12 open/book/cb | 6 | 3 (+3 window +2 'ready'/'update' class events) |
| Window classes | 3 base + 8 furnace + 6 enchant + 3 anvil + 1 villager | furnace 2, enchant 2, villager 2 | — |
| Crafting / recipes | 3 | 0 | 0 |
| Block interaction | 8 | 3 | 4 |
| Scoreboard / teams / bossbar / title | 0 (class methods 11) | 5 | 15 |
| Particles / sound | 0 | 7 (Particle fields) | 4 |
| Time / weather / game | 0 | 20 | 4 |
| Health / food / xp / oxygen | 0 | 7 | 5 |
| Bed / sleep | 4 | 1 | 2 |
| Fishing | 1 | 0 | 0 |
| Settings | 1 | 11 | 0 |
| Resource packs | 2 | 0 | 1 |
| Creative | 6 | 0 | 0 |
| Module-level | 2 (createBot, supportFeature) | 4 classes + 3 version consts | — |

**Totals: ~96 bot methods (excl. ~21 window-class methods), ~96 bot properties (leaf fields incl. nested namespaces), ~96 bot events** (including `chat:{name}` dynamic events, the deprecated `physicTick` alias, and 2 typed-but-unemitted: `unmatchedMessage`, plus internal transaction events excluded from count).
