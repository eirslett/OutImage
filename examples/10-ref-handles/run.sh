#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
cc="$(need_cc)"
sim="$(sim_bin)"
root="$(repo_root)"
"$cc" -c -o out/keep.o -I"$root/runtime" keep.c
"$sim" compile main.sim --link out/keep.o -o out/prog >/dev/null
./out/prog
