#!/usr/bin/env bash
# Run a command with HARD limits on RAM/CPU/time/processes via cgroup v2
# (systemd-run --user). For compiling and RUNNING Sond/qwen3.6 code without
# risking the machine (OOM, fork-bomb, infinite loops).
#
# Usage: tools/sandbox.sh [--mem 2G] [--cpu 200%] [--time 30] [--tasks 256] -- <cmd...>
set -euo pipefail
MEM=2G; CPU=200%; TIME=30; TASKS=256
while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --mem)   MEM=$2;   shift 2;;
    --cpu)   CPU=$2;   shift 2;;
    --time)  TIME=$2;  shift 2;;
    --tasks) TASKS=$2; shift 2;;
    --) shift; break;;
    *) echo "sandbox: unknown flag $1" >&2; exit 2;;
  esac
done
exec systemd-run --user --scope -q --collect \
  -p MemoryMax="$MEM" -p MemorySwapMax=0 \
  -p CPUQuota="$CPU" -p TasksMax="$TASKS" -p RuntimeMaxSec="$TIME" \
  -- "$@"
