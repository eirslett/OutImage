#!/usr/bin/env bash
# Smoke-test examples that can run headlessly.
set -euo pipefail
cd "$(dirname "$0")"

fail=0
run() {
  local dir="$1"
  echo "==> $dir"
  if ! (cd "$dir" && chmod +x run.sh && ./run.sh); then
    echo "FAIL $dir" >&2
    fail=1
  fi
}

run 01-c-from-simula
run 02-simula-from-c
run 03-c-host-embed
run 04-libm
if command -v node >/dev/null 2>&1; then
  run 05-js-from-simula
  run 06-js-embeds-simula
else
  echo "skip js examples (no node)"
fi
run 07-rust-host
run 08-simula-with
run 09-utf8-text
run 10-ref-handles
if command -v python3 >/dev/null 2>&1; then
  run 11-python-ctypes
else
  echo "skip python example (no python3)"
fi
echo "12-browser-canvas: compile only"
(cd 12-browser-canvas && chmod +x run.sh && ./run.sh)

if [ "$fail" -ne 0 ]; then
  echo "some examples failed" >&2
  exit 1
fi
echo "all headless examples ok"
