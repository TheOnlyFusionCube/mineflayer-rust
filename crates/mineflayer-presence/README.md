# mineflayer-presence

A keep-online Minecraft client for every release from 1.7.2 (protocol 4) to
26.3 (protocol 777). A session logs in (offline, or online through Mojang or
any Yggdrasil/authlib-injector network), optionally through a SOCKS5,
SOCKS4 or HTTP proxy, answers the server's liveness traffic, respawns on death, follows transfers, signs chat on servers that
require it, and relays chat both ways. It keeps no world state.

```rust,no_run
use mineflayer_presence::{Auth, Command, Options, Status, spawn};

# async fn demo() {
let (handle, mut chat) = spawn(Options {
    auth: Auth::Offline,
    ..Options::new("play.example.net", 25565, "afkbot")
});
let mut status = handle.status();
while status.changed().await.is_ok() {
    if let Status::Online { .. } = *status.borrow() {
        handle.send(Command::Chat("/afk".into())).await;
    }
}
# }
```

Commands are `Chat` (chat, or a command when it starts with `/`), `Swing`,
`Rotate { yaw, pitch }` (degrees, relative), `Sneak(bool)` and `Quit`. The
session's lifecycle is a `watch` channel (`Handle::status`). Chat and health
arrive on a bounded queue that drops lines when it is full. An owner that
stops reading can never stall the connection.

## Memory

Measured on a test server (Xeon Gold 6454S, Linux) with idle offline bots at
spawn.

| Client | Process baseline | Cost of one more bot |
|---|---|---|
| mineflayer-presence, 2 tokio workers, 3 threads | 4.5 MB RSS | about 0.06 MB RSS (11.0, 20.0, 36.6 MB at 100, 250, 500 bots) |
| Java keep-online runner, server on the runner's native version | 210 MB RSS | 0.38 MB live heap plus native, after GC (from the runner's cost model) |
| Java keep-online runner, server older than 1.21.2 (ViaVersion) | 210 MB RSS | about 4.8 MB live heap |
| mineflayer engine (`examples/idle_bots`) | | about 12.4 MB RSS |

That is about 3 to 6 times less per bot than the Java runner on servers of
its native version, about 75 times less on older servers, and a baseline
about 45 times smaller. The Java figures are post-GC live sets and need
headroom above them (the Java runner budgets 2 MB per bot); the presence
figures are RSS with no collector. The 2-hour soak (G6) shows the RSS stays
flat.

## How versions work

`protocol.json` holds one row per protocol: the presence packets and their
layouts, interpreted at run time by `src/protodef.rs`. The session names
fields, and each protocol's layout decides their order, width, and presence.
Fields the session leaves out are encoded as zero values.

`scripts/gen-presence-protocol.py` generates the table:
- **From minecraft-data:** 1.7.6 to 26.1.
- **By alias:** releases minecraft-data lacks take a neighbour's row when
  nothing in the presence subset differs:
  - 1.7.2 to 1.7.5 take 1.7.6's row;
  - 1.9.1 and 1.14.2 take the row both neighbours agree on;
  - 26.1.1 and 26.1.2 share 26.1's protocol.
- **Derived:** 26.2 and 26.3 take their packet ids from the servers' own
  packet reports (`scripts/presence-reports`), plus the layout changes that
  ViaVersion's rewriters list for them.

## Verification

Each gate is a committed script.

| Gate | Script | Result (2026-10-19) |
|---|---|---|
| G1 unit and fuzz | `cargo test -p mineflayer-presence` | 11 tests; 20,000 random inputs and every truncation per schema, no panic |
| G2 differential codec | `scripts/presence-diff/run.sh 1000` | 1,681,000 vectors agree with node-minecraft-protocol 1.68.0 (26.2 and 26.3 have no reference data; G3 covers them) |
| G3 vanilla matrix | `scripts/presence-matrix/matrix.sh` | all 54 releases, 1.7.2 to 26.3, pass |
| G4 compatibility | `scripts/presence-compat/compat.py` | Paper 1.8.8 to 26.1.2; ViaVersion clients 1.7.10 to 26.3; Velocity and BungeeCord switches; transfer; online mode; signed chat from 1.19.3 to 26.3; SOCKS5, SOCKS4 and HTTP proxies with credentials |
| G5 robustness | `scripts/presence-robust/run.sh` | half-open ends in 29 s; owner that never reads; server stop; 50 bots against a hostile server peak at 7.5 MB |
| G6 soak | `examples/soak.rs` | 500 bots for 2 h: 0 unexpected ends, RSS 37.85 MB at 30 min and 37.86 MB at 119 min. Churn, 200 bots with a tenth reconnecting every 60 s for 1 h: 1,360 sessions, 0 unexpected ends, RSS 18.23 MB at 30 min and 18.32 MB at 59 min |

G3 checks each release against a vanilla server. Two bots stay past two
keepalive timeouts. Bot 0 sends:
- chat, and a line with a colour code that must arrive stripped;
- a command, and a `/tell` to bot 1;
- a swing, a turn and a sneak.

RCON says, tellraws, kills bot 0, and says again after the respawn. From
1.13 the server must report the turn. From 1.17 it requires a resource pack.
The suite runs offline, so the ports stay below 32768.

## Limits

- Chat signing covers 1.19.3 and later. On 1.19 to 1.19.2 an enforcing
  server refuses the login: those releases need a key signed in a scheme
  (v1) that this crate does not produce.
- One online 26.1 run (non-enforcing server) rejected a signed `/msg` as
  "invalid signature". Eight repeats with `/tell` and `/msg` did not
  reproduce it, and the signed body and packet match the 26.1 server's
  bytecode. The cause is unknown.
- 26.2 and 26.3 have no differential reference (G2 skips them). Their
  layouts rest on the server packet reports, ViaVersion's rewriters, and the
  live checks.
- Signed commands cover the vanilla commands with message arguments: `msg`,
  `tell`, `w`, `say`, `teammsg`, `tm` and `me`. A plugin command that
  declares a brigadier message argument goes out unsigned, and an
  enforcing server refuses it.
- Resource packs are reported loaded without being downloaded.
- The session sends no movement after teleports. Vanilla servers accept
  that; some anti-cheat plugins may not.
- Forge and NeoForge servers that require a mod handshake refuse the bot,
  as they refuse any vanilla client.
- 26.4 and later need either minecraft-data or a packet report plus a
  ViaVersion layout check (see `scripts/presence-reports`).
