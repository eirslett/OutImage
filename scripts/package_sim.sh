#!/usr/bin/env bash
# Pack the release `sim` binary as sim-<os>-<arch>.tar.gz (zip on Windows).
# Archive contains `sim` / `sim.exe` so it can go straight on PATH.
set -euo pipefail

host=$(rustc -vV | awk '/^host:/{print $2}')
case "$host" in
  aarch64-apple-darwin)      label=macos-arm64 ;;
  x86_64-apple-darwin)       label=macos-x64 ;;
  aarch64-unknown-linux-gnu) label=linux-arm64 ;;
  x86_64-unknown-linux-gnu)  label=linux-x64 ;;
  aarch64-pc-windows-msvc)   label=windows-arm64 ;;
  x86_64-pc-windows-msvc)    label=windows-x64 ;;
  *)
    echo "unmapped rustc host triple: $host" >&2
    exit 1
    ;;
esac

mkdir -p dist
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

if [[ -f target/release/sim.exe ]]; then
  cp target/release/sim.exe "$stage/sim.exe"
  py=python3
  command -v python3 >/dev/null || py=python
  "$py" - "$stage" "dist/sim-${label}.zip" <<'PY'
import sys
import zipfile
from pathlib import Path

stage = Path(sys.argv[1])
archive = Path(sys.argv[2])
with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as zf:
    zf.write(stage / "sim.exe", "sim.exe")
PY
else
  cp target/release/sim "$stage/sim"
  chmod +x "$stage/sim"
  tar -C "$stage" -czf "dist/sim-${label}.tar.gz" sim
fi

ls -l dist
