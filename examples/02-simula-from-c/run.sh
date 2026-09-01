#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
cc="$(need_cc)"
sim="$(sim_bin)"
ext="$(lib_ext)"
"$sim" compile --crate-type lib add.sim -o "out/libadd.$ext" >/dev/null
"$cc" -o out/host host.c "out/libadd.$ext" -Wl,-rpath,"$PWD/out"
./out/host
