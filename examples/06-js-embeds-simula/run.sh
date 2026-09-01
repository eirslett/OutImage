#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
mkdir -p out
sim="$(sim_bin)"
"$sim" compile --target wasm-browser add.sim -o out/add.wasm >/dev/null
node run.mjs out/add.wasm
