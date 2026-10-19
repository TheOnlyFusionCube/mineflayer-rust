#!/usr/bin/env bash
# matrix.sh <parallelism> <versions...> : run check.sh on each release, BIN from env.
# Ports stay below 32768, outside the Linux ephemeral range, so outgoing
# sockets cannot take a server port.
P=$1; shift; HERE=$(cd "$(dirname "$0")" && pwd); OUT=${VT:-$HOME/.cache/presence-matrix}/matrix; mkdir -p $OUT; i=0
for v in "$@"; do
  port=$((20000 + 20*i)); i=$((i+1))
  echo "$v $port"
done | xargs -P $P -n 2 sh -c "$HERE/check.sh \$0 \$1 > $OUT/\$0.log 2>&1; tail -1 $OUT/\$0.log"
