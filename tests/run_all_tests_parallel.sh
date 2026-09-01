#!/usr/bin/env bash
# Run TestBatch result-plan suites (native, interpreter, wasm) in parallel.
# Streams prefixed console output.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

LOG_DIR="${TMPDIR:-/tmp}/outimage-result-plan-logs"
mkdir -p "$LOG_DIR"

if [[ ! -x target/debug/sim ]]; then
  echo "==> building sim (debug)"
  cargo build -q
fi

echo "==> running TestBatch suites in parallel"
START=$(date +%s)

native_log="$LOG_DIR/native.log"
interp_log="$LOG_DIR/interp.log"
wasm_log="$LOG_DIR/wasm.log"

# Stream each suite to the console with a label, and keep an unprefixed log.
run_suite() {
  local label=$1
  local logfile=$2
  shift 2
  # pipefail so a non-zero python exit fails the background job.
  set -o pipefail
  # Unbuffered so progress lines show up as each unit finishes.
  PYTHONUNBUFFERED=1 "$@" 2>&1 \
    | tee "$logfile" \
    | sed -u "s/^/[${label}] /"
}

run_suite native "$native_log" python3 "$ROOT/tests/run_testbatch.py" native &
native_pid=$!
run_suite interp "$interp_log" python3 "$ROOT/tests/run_testbatch.py" interp &
interp_pid=$!
run_suite wasm "$wasm_log" python3 "$ROOT/tests/run_testbatch.py" wasm &
wasm_pid=$!

native_rc=0
interp_rc=0
wasm_rc=0
wait "$native_pid" || native_rc=$?
wait "$interp_pid" || interp_rc=$?
wait "$wasm_pid" || wasm_rc=$?

END=$(date +%s)
ELAPSED=$((END - START))

echo
echo "==> summary (${ELAPSED}s wall)"
echo "    native:       exit $native_rc"
echo "    interpreter:  exit $interp_rc"
echo "    wasm:         exit $wasm_rc"
echo "    logs: $LOG_DIR/{native,interp,wasm}.log"

if (( native_rc != 0 || interp_rc != 0 || wasm_rc != 0 )); then
  exit 1
fi
