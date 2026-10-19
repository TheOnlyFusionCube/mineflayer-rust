#!/usr/bin/env bash
# run.sh [values-per-packet]: build the vector generator, install the pinned
# node-minecraft-protocol, and run the differential check. Exit 0 only when
# every vector agrees.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
N=${1:-1000}
(cd "$HERE" && [ -d node_modules ] || npm install --silent --no-audit --no-fund)
(cd "$ROOT" && cargo build --release -q -p mineflayer-presence --example vectors)
"$ROOT/target/release/examples/vectors" "$N" | node "$HERE/check.js"
