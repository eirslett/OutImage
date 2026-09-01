#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
sim="$(sim_bin)"
"$sim" compile hypot.sim -o out/prog >/dev/null
./out/prog
