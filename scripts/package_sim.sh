#!/usr/bin/env bash
# Copy the release `sim` binary to dist/sim-<rustc-host-triple>[.exe].
set -euo pipefail
host=$(rustc -vV | awk '/^host:/{print $2}')
mkdir -p dist
if [[ -f target/release/sim.exe ]]; then
  cp target/release/sim.exe "dist/sim-${host}.exe"
else
  cp target/release/sim "dist/sim-${host}"
fi
ls -l dist
