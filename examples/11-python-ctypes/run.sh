#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
sim="$(sim_bin)"
ext="$(lib_ext)"
"$sim" compile --crate-type lib add.sim -o "out/libadd.$ext" >/dev/null
python3 host.py "out/libadd.$ext"
