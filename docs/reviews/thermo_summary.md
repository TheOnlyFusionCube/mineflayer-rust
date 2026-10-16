# Thermo-nuclear code quality review — mineflayer-rust (phase `aaec59a..65e0c47`)

Reviewed commit `65e0c473` against `aaec59a` (22 files, +1705/-201), with the
crate's current state in scope. Correctness gates are green and a separate
behavior review approved the code; **this review deliberately judges structure,
not behavior** — and on that bar the phase is not approvable as-is. The
phase-added structure costs are concrete and payable; the rest is a
prioritized simplification inventory that predates the phase.

Detail files: `01_dig_machinery.md` (dig/abort subsystem),
`02_misc_tooling.md` (misc, windows, scripts, live tests),
`03_structure_bootstrap.md` + `03_structure_measurements.json` (bootstrap,
repo-wide decomposition seams, measured line counts).

Measured baseline facts: 23 flat `src/*.rs`, 26,091 physical lines; non-test
line counts put ten modules at or above ~975 lines, with `pathfinder.rs`
(1,885), `window_types.rs` (1,469), `interact.rs` (1,403), `bot.rs` (1,388)
largest. This phase added 317 production lines to `interact.rs`, 42 to
`bot.rs`, 38 to `windows.rs`, 27 to `misc.rs`, plus ~280 lines of live-test
code and two new Python scripts.

## Findings

**D1 — `retire_engine_dig` is a redundant second cancellation layer; delete
it.** The reconciler (`reconcile_aborted_dig`, interact.rs:305) already
cancels a queued or live mine for an aborted generation inside the tick that
matters, and production clients always install it (`join_azalea`,
bot.rs:1173). `retire_engine_dig` (interact.rs:633) is the only other
mechanism, has exactly one production caller, duplicates the same generation
predicate and a second wait-counting scheme, and its two unit tests exercise
a plugin-less world that runs no engine at all. A focused challenger
independently reached the same verdict. The only thing deletion gives up is
having dig()'s abort error wait for a best-effort ECS cleanup barrier — a
barrier nothing depends on, because the in-span pass always runs before the
mining chain can act. Deleting it removes ~78 production lines, 2 tests, and
one of two parallel semantics a reader must reconcile. (See 01, D1.)

**D2 — this phase pushed `interact.rs` further past its seam instead of
splitting it.** The file grew 1,087 -> 1,403 production lines, absorbing the
whole abort subsystem (state, marker, reconciler, plugin, retirement) into a
file that also owns placement geometry, entity placement, raycast helpers,
and block activation. The seam is clean: `interact/dig.rs` for the dig
promise + generation model + its plugin (they must stay together), placement
in a sibling, shared raycast primitives only if both need them. Do not
restructure the cancellation model while extracting — split, then delete
retire per D1. (See 01, D2; 03, "Decomposition".)

**D4 — the in-span guarantee is verified by comments, not by a test.** The
five reconciliation tests call the system function directly against a bare
`World`; nothing catches the plugin being dropped from `join_azalea`, wrong
schedule ordering, or drift from azalea's pinned handler. One network-free
schedule-level test (build the app through the production path, enqueue a
start for an aborted generation, drive Update+GameTick, assert mining never
becomes active) would pin the integration that this phase's whole design
rests on. (See 01, D4; 03, B1.)

**M1 — the live suites are turning fixture non-determinism into test
archaeology.** This phase's live edits add adaptive terrain search (six block
kinds, on-ground waits), per-attempt taps, error collection, and a compass
sweep, because the persistent fixture world has drifted. Root cause: the
fixture has no reset path and an empty seed (`scripts/live-server.sh`, no
reset handling). Give the fixture a fixed seed and a reset command, then
simplify the search blocks back to deterministic assumptions — that deletes
test complexity instead of spreading it. (See 02, M1.)

**B2 — domain-state wiring is duplicated across `Bot`, `BotBridge`, and
`BridgeDeps`.** `Bot::connect_azalea` allocates six domain handles and reason
slots, enumerates them into the bridge bag, and relists them in the returned
`Bot`; `Bot::new` relists them again. A single shared domain-state object
constructed once (keeping per-domain locks and leaving `_alive`/shutdown
ownership outside it) deletes the parallel lists. (See 03, B2.)

**B3 — view distance exists twice.** `BotBuilder.view_distance` and
`ClientInformation.view_distance` are merged in the bridge with a documented
precedence branch ("frozen signature") on a call path that is private and has
one caller. Store it once in `ClientInformation` and delete the parameter,
the merge, and the exception. (See 03, B3.)

**B4 — typed windows keep five parallel maps per window id.** kinds, data,
offers, selected trade, and readiness are cleared/replaced in lockstep at
every open/close/reset. One `HashMap<i32, WindowRecord>` deletes the repeated
cleanup obligations; preserve the absent-vs-empty offer distinction and
pre-open packets. (See 03, B4.)

**M2 — scoreboard state lives inside `MiscState` while its reducer lives in
`scoreboard.rs`.** Move the five scoreboard maps and their dispatch into
scoreboard's own state, and take `Creative`/`CreativeSlotGuard` out of
misc.rs into a private child in the same pass. (See 03, misc.rs; 02, M2.)

**Pre-existing decomposition inventory (execute one at a time).** `bot.rs`:
extract the 623-line `BotEvent` contract and the connection/join code
(`bot/event.rs`, `bot/connection.rs`). `pathfinder.rs`: `goals.rs`,
`movements.rs`, private tracking. `window_types.rs`: per-window-type children.
`windows.rs`: item / tracker / handle. `entity.rs`: tracker, damage.
`world.rs`: tracker reducer. `inventory.rs`: transfer algorithm. Each seam,
with line ranges and constraints to preserve, is in `03_structure_bootstrap.md`.

**Hygiene batch (hour-sized, all verified against the pinned rev).** The
`azalea-chat` direct dependency and its now-false comment can go
(`azalea::chat` re-exports `azalea_chat::*` at the pin); the duplicate
`azalea` dev-dependency is redundant; the misc.rs "uuid isn't in our dep set"
comment contradicts `uuid = "1"`; the 21-entry patch table is correct —
add one sentence on why it covers the whole workspace. (See 02, M3; 03,
"Cargo dependency expression".)

## What is fine (do not churn)

The bootstrap mirror of azalea's `StartClientOpts::new` is faithful at the
pin and necessary (no builder hook returns a `Client` + raw event receiver);
keep it, add the contract note + schedule-level test rather than a generic
plugin framework. The new kick discriminator, the bridge's ordered sequential
observer loop, `clear_inventory`'s `join_all` shape, `CreativeSlotGuard`,
`stop_digging`'s two-branch gate, the reconciler's quiet-run counting, and
`craft.rs`'s dispatch table are all appropriate as written — several earlier
reviews already extracted their simpler forms. No generic event/state/wait
framework is recommended anywhere: the repeated short ECS accessors protect
different types and lifetimes and would cost more than they save.

## Remediation sequence (half-day units, structural first)

1. Delete `retire_engine_dig` + tests; keep `mark_aborted_engine_dig` (D1).
2. Add the schedule-level plugin/order test and the pinned-source contract
   note (D4 + B1). — do 1 and 2 in the same half day; they touch the same area.
3. Extract `interact/dig.rs` (D2).
4. Fixture reset + fixed seed; simplify one live test back to deterministic
   assumptions (M1).
5. B3 view distance deletion (small, deletes a precedence branch).
6. B2 shared domain-state constructor.
7. B4 per-window record; then the decomposition inventory one seam per
   half day, starting with `bot.rs`'s event/connection split.
8. Hygiene batch (M3).

## Verdict

**Not approved on the thermo bar as-is.** The phase's behavior work is sound
and reviewed, but it added a duplicated cancellation layer (D1), grew its host
file instead of splitting at an obvious seam (D2), left its core integration
guarantee untested (D4), and answered fixture drift with adaptive test code
(M1). Items 1–4 are phase-caused and half-day each; with those four landed
the phase would meet the bar, and the remaining items form a healthy backlog
rather than a blocker.


---

## Remediation status (updated after the fix pass)

Landed in `65e0c47..HEAD` (main): D1 (retire deleted; the reconciler is the
single cancellation path), D2 (interact/mod.rs + dig.rs + place.rs), D4+B1
(client_app.rs + a real schedule-level test through the production app),
B2 (BotShared; BridgeOptions reduced to connection policy), B3 (view
distance single source), B4 (WindowRecord), M2 (ScoreboardState +
misc/creative.rs), M3/hygiene (azalea::chat paths, dep cleanup, comments),
M1 (fixture reset + fixed seed + ownership guard; live-suite hardening incl.
a `set_time` decode guard for the disconnect race and a deadlock fix in the
dig test's slot selection), plus the bot/event.rs split and the
ws/win2-merge windows.rs split (`windows/{mod,item,tracker}.rs`).

Acceptance at `e3f541c`: scripts/verify.sh 5/5 PASS; live suite on a reset
world 14/14 and a repeat run without reset 14/14
(/tmp/m1_suite_run3.log, /tmp/m1_suite_run4.log; final head re-run in
/tmp/m1_final_suite.log).

Remaining backlog (review's own sequencing: "one half-day extraction at a
time"): window_types children (furnace/enchant/anvil/villager/book),
entity.rs, world.rs, inventory transfer extraction, pathfinder.rs, and the
bot connection split. Seams are documented in 03_structure_bootstrap.md.

Final state (commit `26c4322`): scripts/verify.sh 5/5 PASS; full live
suite 14/14 on a reset world (/tmp/m1_final_suite3.log), and a
repeat-without-reset run also 14/14 (/tmp/m1_suite_run4.log). All
priority items from the remediation sequence are landed; the remaining
decomposition backlog is listed above and staged per the review's own
sequencing. The final-pass remote reviewer child could not run (provider
account out of funds); the closing check was a controller pass: leftover
references (`retire_engine_dig`, `BridgeDeps`) grep to zero, gates green.
