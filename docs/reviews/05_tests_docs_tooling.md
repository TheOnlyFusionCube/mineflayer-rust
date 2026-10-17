# 05. Tests, docs, and tooling

Baseline `7a2bc95`. Evidence for summary finding F8.

## Tests

| Suite | Count | Where it runs |
|---|---|---|
| Port unit tests | 260 | gate (`cargo test`), CI |
| Port doctests | 9 (compiled `no_run`) | gate, CI |
| Engine tests | 382 | gate (`test (engine)` step), CI |
| Live tests | 14 tests in one binary, `tests/live/` | developer machine only |

The live suite needs a Java 25 runtime and downloads the Minecraft 26.1
server jar from Mojang (`scripts/live-server.sh`). It passed 14 of 14
in this session after the engine move and again after the log fix, each
time on a freshly reset world.

Unit-test line coverage was 68.3% at `2254020` (`docs/coverage.md`). The
lowest modules are the typed windows (`anvil` 5%, `furnace` 20%,
`villager` 32%) and `bot/wait.rs` (0%: its callers are the typed-window
and inventory waits, and no unit test reaches them). The typed windows
need a server-side block to open, so the live suite covers what unit
tests cannot.

Two gaps follow from the engine move. First, the live suite is the only
proof that the port and the engine work together on the wire, and CI
does not run it. A CI job on a schedule (or on manual dispatch) with a
Java setup step, a cache for `.live/` jars, and `scripts/live-server.sh
reset` would close that. Second, the engine proposals in
`01_engine_boundary.md` need engine-level tests. The engine already has a
simulation harness (`mineflayer-client/src/test_utils/simulation.rs`)
that feeds packets into a real app; the inventory and pathfinder fixes
should be tested there.

## Docs

| File | State |
|---|---|
| `README.md` | Current after this session (layout, dependency use, no upstream branding). |
| `docs/api-parity.md` | Current; 526 status rows. Notes refer to engine paths as `mineflayer_engine::…`. |
| `docs/decisions.md` | Current; D-engine added, D-pin superseded. |
| `docs/version_upgrade.md` | Rewritten for upgrades by merge. |
| `docs/performance.md`, `docs/coverage.md` | Current measurements, dated. |
| `docs/PORT-PLAN.md` | Historical; the "never vendored" rule is marked superseded. |
| `docs/surveys/engine-features.md` | Historical. Written against an engine revision older than the base commit (it named a result type that does not exist at `ddef371`). Now marked as such in its header. |
| `docs/surveys/mineflayer-api.md` | Historical survey of the upstream API; the parity table supersedes it. |
| `docs/improvement_plan.md` | Replaced by the new plan. The first plan is in git at `775ba66`. |
| `docs/reviews/` | This audit. The previous thermo reviews were removed; they are in git at `775ba66`. |

The two surveys and `PORT-PLAN.md` are records, not guides. Move them
under `docs/history/` so a reader does not take them for current
instructions.

## Tooling

`scripts/verify.sh` is the gate: fmt, clippy, clippy with `live`, port
tests, engine tests, build, and docs. `.github/workflows/verify.yml` runs
it on every push and pull request. There is no remote yet, so the
workflow has never run.

`scripts/live-server.sh` owns the fixture. It now reads the Minecraft
version from `mineflayer-engine` in `Cargo.lock`.

`scripts/argon-unit.py` and `scripts/test-argon-unit.py` send a brief
plus source files to a local model API and write the returned files into
`src/`, `docs/`, or `tests/`. They are personal workflow tools with no
role in building or testing the crate, and they write into the source
tree from model output. Move them out of the repository, or document
them in the README as maintainer tooling with the write rules they
enforce.

`target/` measured 31 GB at the end of the session.
