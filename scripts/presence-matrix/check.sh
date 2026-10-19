#!/usr/bin/env bash
# check.sh <mc-version> <port> : live presence check of one Minecraft release.
# Env BIN overrides the presence example binary; VT is the cache of server jars and JREs.
# Env SERVER_JAR (with JAVA_MAJOR) runs that jar instead of vanilla, as for Paper;
# PLUGINS is a space-separated list of plugin jars to install; CLIENT_PROTOCOL
# makes the bots speak that protocol (through ViaVersion); NAME names the
# server directory (default: the release). YGGDRASIL=<url> with AUTHLIB_INJECTOR=<jar>
# runs the server online against that Yggdrasil API and logs the bots in there;
# ENFORCE=1 also sets enforce-secure-profile=true. CLIENT_PROXY=<url> connects the
# bots through that proxy.
# Build first: cargo build --release -p mineflayer-presence --example presence Prints VERDICT PASS|FAIL.
set -u
V=$1; PORT=$2; VT=${VT:-$HOME/.cache/presence-matrix}; D=$VT/servers/${NAME:-$V}; HERE=$(cd "$(dirname "$0")" && pwd); BIN=${BIN:-$HERE/../../target/release/examples/presence}
mkdir -p $D && cd $D
if [ -n "${SERVER_JAR:-}" ]; then
  cp "$SERVER_JAR" server.jar && echo "${JAVA_MAJOR:-21}" > javamajor
  mkdir -p plugins && rm -f plugins/*.jar && for p in ${PLUGINS:-}; do cp "$p" plugins/; done
elif [ ! -f server.jar ]; then
  python3 - "$V" <<PY || { echo "VERDICT FAIL (no server jar for $V)"; exit 1; }
import json, sys, urllib.request
v = sys.argv[1]
m = json.load(urllib.request.urlopen("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"))
u = [x["url"] for x in m["versions"] if x["id"] == v][0]
d = json.load(urllib.request.urlopen(u))
open("javamajor", "w").write(str(d.get("javaVersion", {}).get("majorVersion", 8)))
urllib.request.urlretrieve(d["downloads"]["server"]["url"], "server.jar")
PY
fi
# Adoptium publishes JREs for the LTS releases; round up to the next one
J=$(cat javamajor); for lts in 8 11 17 21 25; do [ "$J" -le $lts ] && { J=$lts; break; }; done
JAVA=$VT/jre/$J/bin/java
if [ ! -x $JAVA ]; then
  mkdir -p $VT/jre/$J && curl -sSL "https://api.adoptium.net/v3/binary/latest/$J/ga/linux/x64/jre/hotspot/normal/eclipse" | tar xz -C $VT/jre/$J --strip-components=1
fi
PROTO_NEW=$(python3 -c "v=tuple(int(x) for x in \"$V\".split(\".\")); print(1 if v>=(1,19) else 0)")
FORCED=$(python3 -c "v=tuple(int(x) for x in \"$V\".split(\".\")); print(1 if v>=(1,17) else 0)")
LT=$([ $PROTO_NEW = 1 ] && echo "minecraft\\:flat" || echo FLAT)
echo eula=true > eula.txt
cat > server.properties <<P
server-ip=127.0.0.1
server-port=$PORT
online-mode=$([ -n "${YGGDRASIL:-}" ] && echo true || echo false)
enforce-secure-profile=$([ -n "${ENFORCE:-}" ] && echo true || echo false)
white-list=false
view-distance=4
simulation-distance=4
max-players=50
spawn-protection=0
difficulty=peaceful
level-type=$LT
generate-structures=false
spawn-monsters=false
enable-rcon=true
rcon.port=$((PORT+1000))
rcon.password=pres
P
if [ $FORCED = 1 ]; then printf "resource-pack=http://127.0.0.1:9/pack.zip\nresource-pack-sha1=0123456789abcdef0123456789abcdef01234567\nrequire-resource-pack=true\n" >> server.properties; fi
for p in $PORT $((PORT+1000)); do
  ss -ltnH "sport = :$p" | grep -q . && { echo "VERDICT FAIL $V (port $p already in use)"; exit 1; }
done
# a fresh world each run: a saved player keeps its rotation and position
# from the last run, and the bot's turn is relative to where it spawns
rm -rf server.out world world_nether world_the_end
$JAVA -Xmx2g -Xms512m ${YGGDRASIL:+-javaagent:$AUTHLIB_INJECTOR=$YGGDRASIL} -jar server.jar nogui < /dev/null > server.out 2>&1 &
SPID=$!
for i in $(seq 1 240); do grep -q "Done (" server.out 2>/dev/null && break; kill -0 $SPID 2>/dev/null || break; sleep 1; done
if ! grep -q "Done (" server.out 2>/dev/null; then echo "--- server did not start"; tail -20 server.out; kill $SPID 2>/dev/null; echo "VERDICT FAIL (server start)"; exit 1; fi
# /kill takes a target from 1.8; on 1.7 a drop into the void does the same
KILL=$(python3 -c "v=tuple(int(x) for x in \"$V\".split(\".\")); print('kill pres000' if v>=(1,8) else 'tp pres000 0 -120 0')")
(sleep 20; python3 $HERE/rcon.py $((PORT+1000)) "say rcon-hello" "tellraw @a {\"text\":\"tellraw-hello\"}" "data get entity pres000 Rotation" "$KILL"; sleep 10; python3 $HERE/rcon.py $((PORT+1000)) "say after-respawn") > rcon.out 2>&1 &
timeout 150 $BIN 127.0.0.1 $PORT 2 75 ${CLIENT_PROTOCOL:+--protocol=$CLIENT_PROTOCOL} ${YGGDRASIL:+--yggdrasil=$YGGDRASIL} ${CLIENT_PROXY:+--proxy=$CLIENT_PROXY} > client.out 2>&1; CODE=$?
python3 $HERE/rcon.py $((PORT+1000)) stop > /dev/null 2>&1; for i in $(seq 1 30); do kill -0 $SPID 2>/dev/null || break; sleep 1; done; kill -9 $SPID 2>/dev/null
echo "--- client (exit $CODE)"; grep -v "Health" client.out | head -60
echo "--- rcon"; cat rcon.out
echo "--- server log (bots)"; grep -aE "pres00|lost connection|kick|Exception|ERROR|WARN.*pres|disconnect" server.out | grep -v "Can.t keep up" | head -40
ok=1
[ $CODE = 0 ] || { ok=0; echo "reason: client exit $CODE"; }
grep -q "rcon-hello" client.out || { ok=0; echo "reason: no say chat"; }
grep -q "tellraw-hello" client.out || { ok=0; echo "reason: no tellraw chat"; }
grep -q "after-respawn" client.out || { ok=0; echo "reason: no chat after respawn"; }
grep -aqE "presence check" server.out || { ok=0; echo "reason: server did not log the chat line"; }
grep -aqE "colour ccode stripped" server.out || { ok=0; echo "reason: server did not log the sanitized chat line"; }
grep -q "signed hello" client.out || { ok=0; echo "reason: the bot's /tell to pres001 did not echo back"; }
if [ -n "${ENFORCE:-}" ]; then
  grep -aq "\[Not Secure\] <pres000> presence check" server.out && { ok=0; echo "reason: chat arrived unsigned on an enforcing server"; }
  grep -aqE "<pres000> presence check" server.out || { ok=0; echo "reason: enforcing server did not accept the signed chat"; }
fi
DATA_CMD=$(python3 -c "v=tuple(int(x) for x in \"$V\".split(\".\")); print(1 if v>=(1,13) else 0)")
# the yaw turns from wherever the server spawned the bot, but pitch starts
# at 0 everywhere, so -10 can only come from the bot's look packet
if [ $DATA_CMD = 1 ]; then grep -qE ", -10\.0f\]" rcon.out || { ok=0; echo "reason: rotation not applied server-side ($(grep Rotation rcon.out | cut -c1-120))"; }; fi
[ $ok = 1 ] && echo "VERDICT PASS $V" || echo "VERDICT FAIL $V"
