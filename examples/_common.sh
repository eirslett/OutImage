# Shared helpers for example `run.sh` scripts. Sourced, not executed.

_EXAMPLES_COMMON_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_REPO_ROOT="$(cd "$_EXAMPLES_COMMON_DIR/.." && pwd)"

need_cc() {
  for name in ${CC:+"$CC"} cc clang gcc; do
    if command -v "$name" >/dev/null 2>&1; then
      echo "$name"
      return 0
    fi
  done
  echo "error: need a C compiler (cc, clang, or gcc)" >&2
  exit 1
}

sim_bin() {
  if [ -n "${SIM:-}" ]; then
    echo "$SIM"
    return 0
  fi
  if [ -x "$_REPO_ROOT/target/debug/sim" ]; then
    echo "$_REPO_ROOT/target/debug/sim"
  elif [ -x "$_REPO_ROOT/target/release/sim" ]; then
    echo "$_REPO_ROOT/target/release/sim"
  else
    echo "error: build sim first (cargo build), or set SIM=/path/to/sim" >&2
    exit 1
  fi
}

lib_ext() {
  case "$(uname -s)" in
    Darwin) echo dylib ;;
    MINGW*|MSYS*|CYGWIN*|Windows_NT) echo dll ;;
    *) echo so ;;
  esac
}

repo_root() {
  echo "$_REPO_ROOT"
}

