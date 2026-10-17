# Checklist: the next engine and Minecraft version

Language: ASD-STE100 (Simplified Technical English).

Use this checklist when you move the engine in `crates/` to a new Minecraft
version. Do the steps in this sequence. Do not continue to the next step
until the current step passes.

The engine is a renamed copy of an upstream project (`LICENSE`,
"Third-party notices", gives the project and the base commit `ddef371`).
`docs/decisions.md` (D-engine) gives the rename table.

## 1. Prepare

1. Clone the upstream project into `ref/` and check out the base commit.
2. Read the upstream change log from the base commit to the new revision.
3. Make a diff of the upstream source from the base commit to the new
   revision.
4. Apply the rename table in D-engine to the diff.
5. Apply the renamed diff to `crates/`. Resolve each conflict by hand.
   Keep the local changes (clippy fixes, the `McBuf` derive shorthand).
6. Do not run a code generator. The repository does not contain one
   (D-codegen). The registry, block, item component, entity metadata,
   language, and packet files come with the renamed diff. Do not edit these
   files by hand unless the change is in the D-engine list.
7. Examine each local change in the D-engine list in `docs/decisions.md`.
   Apply the change again if the renamed diff removed it.
8. Change the base commit in `LICENSE` and in D-engine.
9. Change `[workspace.package] version` in `Cargo.toml` to the new engine
   version (the `+mcX.Y` part gives the Minecraft version).

## 2. Compile

1. Run `cargo build`.
2. If the compiler stops with an internal compiler error, do the toolchain
   review in plan item H2 first.
3. Correct each compile error. Record each renamed engine path in the
   commit message.

## 3. Bootstrap

1. Make sure that `ClientPlugins` in
   `crates/mineflayer-engine/src/client_impl/mod.rs` still contains each
   default plugin group, and that `StartClientOpts::new_with_plugins` still
   builds the app from its argument (local engine change 12).
2. Make sure that each plugin that `client_plugins` in
   `src/bot/connection.rs` disables still exists in the engine.
3. Make sure that the start handler in
   `crates/mineflayer-client/src/plugins/mining.rs` still skips the
   start messages that `CancelMiningEvent` cut off (local engine change
   13). Run `cargo test -p mineflayer-client --features log --test main cancel_mining`.

## 4. Packet decode

The bridge decodes these clientbound packets itself (The engine drops or
internalizes them). For each packet, compare the fields that the port reads
with the new `mineflayer-protocol` definition:

| Owner | Packets |
|---|---|
| `bridge.rs` | `Login`, `Respawn`, `SetHealth`, `SetActionBarText`, `TabList`, `CommandSuggestions` |
| `entity/tracker.rs` | `AddEntity`, `RemoveEntities`, `MoveEntityPos`, `MoveEntityPosRot`, `MoveEntityRot`, `TeleportEntity`, `EntityPositionSync`, `RotateHead`, `SetEntityData`, `Animate`, `EntityEvent`, `DamageEvent`, `SetEquipment`, `UpdateAttributes`, `UpdateMobEffect`, `RemoveMobEffect`, `TakeItemEntity`, `SetEntityLink`, `SetPassengers`, `ChangeDifficulty`, `PlayerInfoUpdate`, `PlayerInfoRemove`, `Respawn` |
| `world/tracker.rs` | `BlockUpdate`, `SectionBlocksUpdate`, `ForgetLevelChunk`, `SetTime`, `GameEvent`, `ChangeDifficulty`, `Login`, `Respawn`, `SetDefaultSpawnPosition`, `CustomPayload`, `OpenSignEditor`, `BlockEntityData`, `BlockDestruction`, `BlockEvent` |
| `windows/tracker.rs` | `OpenScreen`, `ContainerSetContent`, `ContainerSetSlot`, `ContainerClose`, `SetHeldSlot`, `SetPlayerInventory`, `EntityEvent`, `Cooldown`, `Login`, `Respawn` |
| `window_types/mod.rs` | `OpenScreen`, `ContainerSetData`, `MerchantOffers`, `ContainerClose`, `Login`, `Respawn` |
| `scoreboard.rs` | `SetObjective`, `SetScore`, `ResetScore`, `SetDisplayObjective`, `SetPlayerTeam`, `BossEvent`, `Login`, `Respawn` |
| `misc/mod.rs`, `misc/creative.rs` | `GameEvent`, `SetHealth`, `SetExperience`, `ResourcePackPush`, `SetTitleText`, `SetSubtitleText`, `SetTitlesAnimation`, `ClearTitles`, `Sound`, `SoundEntity`, `LevelParticles`, `BlockEvent`, `AddEntity`, `RemoveEntities`, `ContainerSetSlot` |
| `movement.rs` | `PlayerPosition`, `PlayerAbilities` |
| `craft.rs` | `RecipeBookAdd`, `RecipeBookRemove` |

1. Search the new the engine for packets that the engine now handles itself. Remove
   the port's decode where the engine now does the same work.
2. Search for new packets that a `diverges` row in `docs/api-parity.md`
   waits for.
3. Examine `windows/tracker.rs` `patch_state_id`. Remove it if the engine now
   adopts `state_id` from `ContainerSetContent`.

## 5. Fixture server

1. `scripts/live-server.sh` reads the Minecraft version from the engine
   version in `Cargo.lock` (`0.16.0+mc26.1` gives `26.1`) and downloads that
   server jar. If the new the engine version string has a different form,
   correct the parse in the script.
2. Run `bash scripts/live-server.sh reset`. Make sure that
   `.live/version.txt` shows the new version.
3. Make sure that the reset output shows the gamerule reply
   (`rcon> gamerule respawn_radius 0: …is now set to: 0`). If the new
   version renames the rule again, add the new name to the `rcon` calls in
   the script.

## 6. Verify

1. Run `scripts/verify.sh`. All eight steps must pass.
2. Run the full live suite on a reset world:
   `cargo test --features live -- --ignored --test-threads=1`.
3. Run each example against the fixture one time
   (`cargo run --example echo -- localhost 25566`).

## 7. Documents

1. Update the version rows in `docs/api-parity.md` (`bot.version`,
   `latestSupportedVersion`, the global divergence note).
2. Update the README "Limitations" section and the version numbers in the
   README.
3. Record the upgrade and any new ruling in `docs/decisions.md`.
