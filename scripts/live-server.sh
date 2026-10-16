#!/usr/bin/env bash
# live-server.sh — local offline-mode vanilla Minecraft server fixture.
#
# Resolves the MC version azalea targets from Cargo.lock
# (`azalea 0.16.0+mc26.1` -> `26.1`), downloads that release's server jar
# from Mojang's piston-meta manifest into .live/, and boots it with
# online-mode=false on port 25566. If the system java is older than the
# release's required `javaVersion`, a matching JRE is vendored into
# .live/jdk/ from Adoptium.
#
# Usage:
#   ./scripts/live-server.sh        # start (idempotent), prints LIVE_SERVER_READY
#   ./scripts/live-server.sh stop   # kill the fixture server
#   ./scripts/live-server.sh reset  # stop, delete the worlds, start with seed 13371337
#
# State lives in .live/ (gitignored): server.jar, server.log, server.pid,
# version.txt, eula.txt, server.properties, jdk/, world*.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIVE_DIR="$ROOT/.live"
PORT=25566
RCON_PORT=25576
MANIFEST_URL="https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
BOOT_TIMEOUT_SECS=120

mkdir -p "$LIVE_DIR"

json_pick() {
    # json_pick <file|-> <jq-expr> — uses jq if present, else python3.
    local expr="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r "$expr" "$1"
    else
        python3 - "$1" "$expr" <<'PYEOF'
import json, sys
data = json.load(open(sys.argv[1]) if sys.argv[1] != "-" else sys.stdin)
expr = sys.argv[2]
# minimal jq-subset for the exprs used below
if expr == ".latest.release":
    print(data["latest"]["release"] or "")
elif expr == "[.versions[] | select(.type == \"release\") | .id] | .[]":
    for v in data["versions"]:
        if v["type"] == "release":
            print(v["id"])
elif expr.startswith(".versions"):
    # '.versions[] | select(.id == "X") | .url'
    vid = expr.split('"')[1]
    for v in data["versions"]:
        if v["id"] == vid:
            print(v["url"])
elif expr == ".downloads.server.url":
    print(data["downloads"]["server"]["url"])
elif expr == ".javaVersion.majorVersion":
    print(data.get("javaVersion", {}).get("majorVersion", ""))
else:
    sys.exit("unsupported expr: " + expr)
PYEOF
    fi
}

mc_target_version() {
    # azalea version string in Cargo.lock, e.g. 0.16.0+mc26.1 -> 26.1
    # Cargo.lock line is `version = "0.16.0+mc26.1"` — fields: version = $3.
    awk '
        /^name = "azalea"$/ { found = 1 }
        found && /^version/ {
            v = $3
            gsub(/"/, "", v)
            sub(/.*\+mc/, "", v)
            print v
            exit
        }
    ' "$ROOT/Cargo.lock"
}

pick_version() {
    # Echoes the manifest release id to boot. Prefers an exact match for the
    # azalea-targeted MC version; else the nearest same-major release; else
    # (with a loud mismatch note) the newest release listed.
    local target="$1" manifest="$2" rels best=""
    rels="$(json_pick "$manifest" '[.versions[] | select(.type == "release") | .id] | .[]')"

    if printf '%s\n' "$rels" | grep -qx "$target"; then
        echo "$target"; return
    fi

    # nearest same-major release: smallest 26.x >= target, else largest < target.
    # Manifest order is newest-first; awk string-compare is fine within a major.
    local same_major
    same_major="$(printf '%s\n' "$rels" | grep "^${target%%.*}\." || true)"
    if [ -n "$same_major" ]; then
        best="$(printf '%s\n' "$same_major" | awk -v t="$target" '
            NR == 1 { first = $0 }
            $0 >= t { cand = $0 }
            END { print (cand != "" ? cand : first) }')"
        echo "$best"
        echo "note: exact MC $target not a published release; using nearest same-major $best" >&2
        return
    fi

    best="$(json_pick "$manifest" '.latest.release')"
    echo "warning: no ${target%%.*}.x release in piston-meta; falling back to latest release $best — PROTOCOL MISMATCH with azalea mc$target" >&2
    echo "$best"
}

java_major() {
    # java_major <path-to-java-binary> -> major version number or empty
    local v
    v="$("$1" -version 2>&1 | awk -F '"' '/version/ {print $2; exit}')"
    [ -z "$v" ] && { echo ""; return; }
    if [[ "$v" == 1.* ]]; then
        # "1.8.0_431" -> 8
        v="${v#1.}"
    fi
    echo "${v%%.*}"
}

resolve_java() {
    # resolve_java <required-major> -> path to a java binary meeting it.
    local need="$1" sys=""
    if command -v java >/dev/null 2>&1; then
        local m
        m="$(java_major "$(command -v java)")"
        [ -n "$m" ] && [ "$m" -ge "$need" ] && { command -v java; return; }
        echo "note: system java is ${m:-unknown} but MC $PICKED_VERSION requires $need" >&2
    fi

    # previously vendored JRE?
    local vendored
    vendored="$(find "$LIVE_DIR/jdk" -type f -path '*/bin/java' 2>/dev/null | head -1)"
    if [ -n "$vendored" ]; then
        local vm
        vm="$(java_major "$vendored")"
        [ -n "$vm" ] && [ "$vm" -ge "$need" ] && { echo "$vendored"; return; }
    fi

    local arch
    case "$(uname -m)" in
        arm64|aarch64) arch="aarch64" ;;
        x86_64|amd64)  arch="x64" ;;
        *) echo "error: unsupported arch $(uname -m)" >&2; exit 1 ;;
    esac

    echo "downloading Temurin JRE $need ($arch) into .live/jdk/" >&2
    mkdir -p "$LIVE_DIR/jdk"
    rm -f "$LIVE_DIR/jdk.tar.gz"
    curl -fsSL --max-time 600 \
        "https://api.adoptium.net/v3/binary/latest/$need/ga/mac/$arch/jre/hotspot/normal/eclipse" \
        -o "$LIVE_DIR/jdk.tar.gz"
    tar -xzf "$LIVE_DIR/jdk.tar.gz" -C "$LIVE_DIR/jdk"
    rm -f "$LIVE_DIR/jdk.tar.gz"

    vendored="$(find "$LIVE_DIR/jdk" -type f -path '*/bin/java' | head -1)"
    [ -n "$vendored" ] || { echo "error: vendored JRE has no bin/java" >&2; exit 1; }
    echo "$vendored"
}

pid_cwd() {
    lsof -a -p "$1" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p'
}

pid_alive() {
    [ -f "$LIVE_DIR/server.pid" ] || return 1
    local pid
    pid="$(cat "$LIVE_DIR/server.pid")"
    kill -0 "$pid" 2>/dev/null && [ "$(pid_cwd "$pid")" = "$LIVE_DIR" ]
}

assert_port_free() {
    command -v lsof >/dev/null 2>&1 || { echo "error: lsof is needed to check port $PORT" >&2; exit 1; }
    local pid cwd
    pid="$(lsof -nP -t -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | sed -n '1p' || true)"
    [ -z "$pid" ] && return
    cwd="$(pid_cwd "$pid")"
    echo "error: port $PORT already occupied by pid $pid (cwd ${cwd:-unknown}); not starting fixture from $LIVE_DIR" >&2
    exit 1
}

stop_server() {
    if pid_alive; then
        local pid
        pid="$(cat "$LIVE_DIR/server.pid")"
        kill "$pid" 2>/dev/null || true
        for _ in $(seq 1 20); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.5
        done
        kill -9 "$pid" 2>/dev/null || true
        rm -f "$LIVE_DIR/server.pid"
        echo "stopped server (pid $pid)"
    else
        rm -f "$LIVE_DIR/server.pid"
        echo "no running server"
    fi
}

start_server() {
    if pid_alive; then
        echo "server already running (pid $(cat "$LIVE_DIR/server.pid"))"
        echo "LIVE_SERVER_READY"
        return
    fi
    assert_port_free

    local target version version_url version_json jar_url required_java java_bin
    target="$(mc_target_version)"
    [ -n "$target" ] || { echo "error: could not parse azalea version from Cargo.lock" >&2; exit 1; }

    curl -fsSL --max-time 30 "$MANIFEST_URL" -o "$LIVE_DIR/version_manifest.json"
    version="$(pick_version "$target" "$LIVE_DIR/version_manifest.json")"
    PICKED_VERSION="$version"
    echo "$version" > "$LIVE_DIR/version.txt"
    echo "azalea targets mc$target; booting server $version (port $PORT, offline mode)"

    # version JSON: server jar url + required java major
    version_url="$(json_pick "$LIVE_DIR/version_manifest.json" ".versions[] | select(.id == \"$version\") | .url")"
    curl -fsSL --max-time 30 "$version_url" -o "$LIVE_DIR/version.json"
    jar_url="$(json_pick "$LIVE_DIR/version.json" '.downloads.server.url')"
    [ -n "$jar_url" ] && [ "$jar_url" != "null" ] || { echo "error: release $version has no server jar in manifest" >&2; exit 1; }
    required_java="$(json_pick "$LIVE_DIR/version.json" '.javaVersion.majorVersion')"
    required_java="${required_java:-21}"

    java_bin="$(resolve_java "$required_java")"
    [ -n "$java_bin" ] || { echo "error: no java >= $required_java available" >&2; exit 1; }
    echo "using java: $java_bin ($("$java_bin" -version 2>&1 | head -1))"

    if [ ! -f "$LIVE_DIR/server.jar" ] || [ "$(cat "$LIVE_DIR/jar-version.txt" 2>/dev/null)" != "$version" ]; then
        echo "downloading $jar_url"
        curl -fsSL --max-time 300 "$jar_url" -o "$LIVE_DIR/server.jar"
        echo "$version" > "$LIVE_DIR/jar-version.txt"
    fi

    printf 'eula=true\n' > "$LIVE_DIR/eula.txt"
    # RCON is for this script only: bound to localhost (server-ip) with a
    # fresh random password per start, written only into the ignored .live/.
    rcon_password="$(python3 -c 'import secrets; print(secrets.token_hex(16))')"
    cat > "$LIVE_DIR/server.properties" <<EOF
online-mode=false
server-ip=127.0.0.1
server-port=$PORT
level-seed=13371337
sync-chunk-writes=false
spawn-protection=0
enable-rcon=true
rcon.port=$RCON_PORT
rcon.password=$rcon_password
EOF

    : > "$LIVE_DIR/server.log"
    (cd "$LIVE_DIR" && exec "$java_bin" -jar server.jar nogui) >> "$LIVE_DIR/server.log" 2>&1 &
    local pid=$!
    echo "$pid" > "$LIVE_DIR/server.pid"

    local elapsed=0
    while [ "$elapsed" -lt "$BOOT_TIMEOUT_SECS" ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "error: server process died during boot; last log lines:" >&2
            tail -20 "$LIVE_DIR/server.log" >&2
            rm -f "$LIVE_DIR/server.pid"
            exit 1
        fi
        if grep -q 'Done (' "$LIVE_DIR/server.log" 2>/dev/null; then
            grep 'Done (' "$LIVE_DIR/server.log" | tail -1
            # every join at the world spawn: vanilla scatters joins over a
            # 21x21 area (spawn radius 10), which made the live tests depend
            # on random terrain (E5)
            # (MC 26.1 names the rule respawn_radius; older names as fallback)
            rcon "gamerule respawn_radius 0" || rcon "gamerule spawnRadius 0" || {
                echo "error: could not set the spawn radius over RCON" >&2
                exit 1
            }
            echo "LIVE_SERVER_READY"
            return
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    echo "error: server did not reach 'Done' within ${BOOT_TIMEOUT_SECS}s; last log lines:" >&2
    tail -20 "$LIVE_DIR/server.log" >&2
    exit 1
}

# Send one console command over RCON (localhost only); print the reply.
# Fails when the server reports an unknown or incorrect command.
rcon() {
    python3 - "$RCON_PORT" "$(sed -n 's/^rcon.password=//p' "$LIVE_DIR/server.properties")" "$1" <<'PY'
import socket, struct, sys, time
port, password, command = int(sys.argv[1]), sys.argv[2], sys.argv[3]
def packet(req, kind, body):
    data = struct.pack('<ii', req, kind) + body.encode() + b'\x00\x00'
    return struct.pack('<i', len(data)) + data
def read(sock):
    size = struct.unpack('<i', sock.recv(4))[0]
    data = b''
    while len(data) < size:
        data += sock.recv(size - len(data))
    req, _ = struct.unpack('<ii', data[:8])
    return req, data[8:-2].decode(errors='replace')
for attempt in range(20):
    try:
        sock = socket.create_connection(('127.0.0.1', port), timeout=5)
        break
    except OSError:
        time.sleep(0.5)
else:
    sys.exit('rcon: no connection')
sock.sendall(packet(1, 3, password))
if read(sock)[0] == -1:
    sys.exit('rcon: login refused')
sock.sendall(packet(2, 2, command))
reply = read(sock)[1]
print(f'rcon> {command}: {reply}')
low = reply.lower()
sys.exit(1 if ('unknown' in low or 'incorrect' in low or 'error' in low) else 0)
PY
}

case "${1:-start}" in
    start) start_server ;;
    stop)  stop_server ;;
    reset)
        stop_server
        assert_port_free
        rm -rf "$LIVE_DIR/world" "$LIVE_DIR/world_nether" "$LIVE_DIR/world_the_end"
        start_server
        ;;
    *) echo "usage: $0 [start|stop|reset]" >&2; exit 2 ;;
esac
