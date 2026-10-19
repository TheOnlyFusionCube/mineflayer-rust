#!/usr/bin/env bash
# stop.sh: stop every process group running from the matrix cache ($VT) or
# this checkout (servers, bots, check.sh, compat.py), never the caller's own
# group. Matching on command lines instead would also match the shell that
# runs the match.
VT=${VT:-$HOME/.cache/presence-matrix}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
me=$(ps -o pgid= -p $$ | tr -d ' ')
for round in 1 2 3 4 5; do
  groups=$(for pid in $(ps -eo pid=); do
    case "$(readlink /proc/$pid/cwd 2>/dev/null)" in
      ("$VT"*|"$ROOT"*) ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ' ;;
    esac
  done | sort -u | grep -vx "$me")
  [ -z "$groups" ] && { echo "stopped"; exit 0; }
  for g in $groups; do kill -TERM -- "-$g" 2>/dev/null; done
  sleep 4
done
echo "still running: $groups"; exit 1
