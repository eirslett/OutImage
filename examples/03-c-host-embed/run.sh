#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
cc="$(need_cc)"
sim="$(sim_bin)"
root="$(repo_root)"
ext="$(lib_ext)"
"$sim" compile --crate-type lib model.sim -o "out/libmodel.$ext" >/dev/null
"$cc" -o out/host -I"$root/runtime" host.c "out/libmodel.$ext" -Wl,-rpath,"$PWD/out"
./out/host
