#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
cc="$(need_cc)"
sim="$(sim_bin)"
"$cc" -c -o out/greet.o greet.c
"$sim" compile --charset utf8 main.sim --link out/greet.o -o out/prog >/dev/null
./out/prog
