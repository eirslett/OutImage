#!/usr/bin/env bash
set -euo pipefail
. "$(dirname "$0")/../_common.sh"
cd "$(dirname "$0")"
sim="$(sim_bin)"
"$sim" compile --target wasm-browser model.sim -o model.wasm >/dev/null
echo "Compiled model.wasm and wasm_host.mjs. From this directory run:"
echo "  python3 -m http.server 8765"
echo "then open http://127.0.0.1:8765/"
