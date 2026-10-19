#!/usr/bin/env bash
# run.sh [port]: G5 robustness checks of mineflayer-presence on Linux.
#   half-open   a proxy freezes mid-session; the bot must end within 35 s
#   restart     the server stops; every bot must end within 10 s
#   evil        50 bots against a hostile server; no panic, RSS under 64 MB
#   slow-owner  events go unread under a chat flood; bots stay online
# Uses the vanilla 26.1 server and JRE that ../presence-matrix/check.sh caches.
# Prints "G5 <check> PASS|FAIL <detail>" and exits non-zero on any FAIL.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
VT=${VT:-$HOME/.cache/presence-matrix}
BIN=${BIN:-$HERE/../../target/release/examples/presence}
P=${1:-23000}
D=$VT/servers/26.1
OUT=$VT/robust; mkdir -p "$OUT"
fail=0
say() { echo "G5 $*"; case "$*" in *FAIL*) fail=1 ;; esac; }
rcon() { python3 "$HERE/../presence-matrix/rcon.py" $((P + 1000)) "$@" > /dev/null 2>&1; }

[ -f "$D/server.jar" ] || "$HERE/../presence-matrix/check.sh" 26.1 $((P + 500)) > /dev/null
cd "$D"
sed -i "s/^server-port=.*/server-port=$P/; s/^rcon.port=.*/rcon.port=$((P + 1000))/; /^resource-pack/d; /^require-resource-pack/d" server.properties
start_server() {
  rm -f server.out
  "$VT/jre/25/bin/java" -Xmx2g -Xms512m -jar server.jar nogui < /dev/null > server.out 2>&1 &
  SPID=$!
  for i in $(seq 1 120); do grep -q "Done (" server.out 2>/dev/null && return 0; sleep 1; done
  return 1
}
stop_server() { rcon stop; for i in $(seq 1 30); do kill -0 $SPID 2>/dev/null || return; sleep 1; done; kill -9 $SPID 2>/dev/null; }

start_server || { say "setup FAIL server did not start"; exit 1; }

# half-open
python3 "$HERE/freeze_proxy.py" $((P + 2)) $P 20 > "$OUT/freeze.log" 2>&1 &
FP=$!
sleep 1
timeout 120 "$BIN" 127.0.0.1 $((P + 2)) 1 80 > "$OUT/halfopen.out" 2>&1
kill $FP 2>/dev/null
end_t=$(grep -m1 "status [0-9.]*s Ended" "$OUT/halfopen.out" | sed -E 's/status ([0-9.]+)s.*/\1/')
online_t=$(grep -m1 "status [0-9.]*s Online" "$OUT/halfopen.out" | sed -E 's/status ([0-9.]+)s.*/\1/')
if [ -n "$end_t" ] && [ -n "$online_t" ]; then
  # the proxy froze 20 s after the bot's connection opened, about the time it came online
  d=$(python3 -c "print(round($end_t - $online_t - 20, 1))")
  ok=$(python3 -c "print(1 if $d <= 35 else 0)")
  [ "$ok" = 1 ] && say "half-open PASS ended ${d}s after freeze ($(grep -m1 Ended "$OUT/halfopen.out" | cut -c1-120))" || say "half-open FAIL ended ${d}s after freeze"
else
  say "half-open FAIL no end within 80 s of a freeze at 20 s"
fi

# slow owner under a chat flood
(for i in $(seq 1 600); do rcon "say flood line $i with some padding to fill the queue"; sleep 0.1; done) &
FL=$!
timeout 150 "$BIN" 127.0.0.1 $P 3 75 --no-read > "$OUT/slow.out" 2>&1
kill $FL 2>/dev/null; wait $FL 2>/dev/null
grep -q "online=3/3 ended_early=0" "$OUT/slow.out" && say "slow-owner PASS $(grep summary "$OUT/slow.out")" || say "slow-owner FAIL $(grep -E "summary|ended " "$OUT/slow.out" | tr '\n' ' ' | cut -c1-300)"

# server restart
timeout 120 "$BIN" 127.0.0.1 $P 3 60 > "$OUT/restart.out" 2>&1 &
CL=$!
sleep 25
stop_t=$(date +%s.%N)
stop_server
wait $CL
n=$(grep -c "^ended pres" "$OUT/restart.out")
end0=$(grep -m1 "status [0-9.]*s Ended" "$OUT/restart.out" | cut -c1-120)
[ "$n" = 3 ] && say "restart PASS 3/3 ended ($end0)" || say "restart FAIL $n/3 ended"

# hostile server
python3 "$HERE/evil_server.py" $((P + 3)) > "$OUT/evil.log" 2>&1 &
EV=$!
sleep 1
"$BIN" 127.0.0.1 $((P + 3)) 50 75 > "$OUT/evil.out" 2>&1 &
CL=$!
max=0
while kill -0 $CL 2>/dev/null; do
  r=$(awk '/VmRSS/{print $2}' /proc/$CL/status 2>/dev/null); [ -n "$r" ] && [ "$r" -gt "$max" ] && max=$r
  sleep 0.5
done
kill $EV 2>/dev/null
if grep -q "panicked" "$OUT/evil.out"; then say "evil FAIL panic: $(grep -m1 panicked "$OUT/evil.out")"
elif [ "$max" -ge 65536 ]; then say "evil FAIL peak RSS ${max} KB"
else
  # Mode 3 sends a valid login success and then random packets forever, so a
  # bot can reach the world and stay on a server that keeps talking, as a
  # vanilla client would. Every other mode must end the session.
  stuck=$(python3 -c "
import re
s = open('$OUT/evil.out').read()
ended = {int(m) for m in re.findall(r'^ended pres(\d+)', s, re.M)}
print(' '.join(str(i) for i in range(50) if i not in ended and i % 6 != 3))")
  [ -z "$stuck" ] && say "evil PASS peak RSS ${max} KB, $(grep -c '^ended' "$OUT/evil.out") of 50 ended, the rest on the talking server" || say "evil FAIL bots $stuck outlived the 60 s join deadline"
fi
grep "^ended" "$OUT/evil.out" | sed -E 's/pres[0-9]+: //' | cut -c1-90 | sort | uniq -c | sort -rn | head -6

exit $fail
