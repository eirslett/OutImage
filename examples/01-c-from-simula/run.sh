#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
cc="$(need_cc)"
sim="$(sim_bin)"
"$cc" -c -o out/add.o add.c
"$sim" compile main.sim --link out/add.o -o out/prog >/dev/null
./out/prog
