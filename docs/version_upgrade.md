# Checklist: the next azalea and Minecraft version

Language: ASD-STE100 (Simplified Technical English).

Use this checklist when you move the azalea pin to a new revision or to a
new Minecraft version. Do the steps in this sequence. Do not continue to
the next step until the current step passes.

**WARNING:** Do not remove the `[patch.crates-io]` table in `Cargo.toml`
before you read `docs/decisions.md` (D-pin). If the new azalea release does
not contain commit `ddef371`, each `bot.attack()` call causes a server kick.

## 1. Prepare

1. Read the azalea `CHANGELOG.md` from the old revision to the new revision.
2. Move `ref/azalea` to the new revision
   (`git -C ref/azalea checkout <rev>`). Update the revision in the README
   "Toolchain" section.
3. Change the azalea revision in all 21 `[patch.crates-io]` entries, or
   change `azalea = "…"` and remove the table (plan item H1).
4. Run `cargo update -p azalea`. Make sure that `Cargo.lock` has exactly one
   source for each `azalea*` crate.

## 2. Compile

1. Run `cargo build`.
2. If the compiler stops with an internal compiler error, do the toolchain
   review in plan item H2 first.
3. Correct each compile error. Record each renamed azalea path in the
   commit message.

## 3. Bootstrap mirror

1. Compare `src/client_app.rs` (`app()`) with azalea's
   `azalea/src/client_impl/mod.rs` (`StartClientOpts::new`) at the new
   revision. The default plugin groups and their sequence must be the same.
2. Compare `join_azalea` in `src/bot/connection.rs` with azalea's
   `Client::join` and `Client::start_client`.
3. Run the schedule-level test:
   `cargo test --lib game_tick_cancels_an_aborted_dig_through_the_production_app`.
4. Examine `reconcile_aborted_dig` in `src/interact/dig.rs` against azalea's
   `azalea-client/src/plugins/mining.rs`. The reconciler must still run
   before `MiningSystems`.

## 4. Packet decode

The bridge decodes these clientbound packets itself (azalea drops or
internalizes them). For each packet, compare the fields that the port reads
with the new `azalea-protocol` definition:

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

1. Search the new azalea for packets that azalea now handles itself. Remove
   the port's decode where azalea now does the same work.
2. Search for new packets that a `diverges` row in `docs/api-parity.md`
   waits for.
3. Examine `windows/tracker.rs` `patch_state_id`. Remove it if azalea now
   adopts `state_id` from `ContainerSetContent`.

## 5. Fixture server

1. `scripts/live-server.sh` reads the Minecraft version from the azalea
   version in `Cargo.lock` (`0.16.0+mc26.1` gives `26.1`) and downloads that
   server jar. If the new azalea version string has a different form,
   correct the parse in the script.
2. Run `bash scripts/live-server.sh reset`. Make sure that
   `.live/version.txt` shows the new version.

## 6. Verify

1. Run `scripts/verify.sh`. All six steps must pass.
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
