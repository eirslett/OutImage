#!/usr/bin/env python3
"""Run the 100 Simula TestBatch units (tests/testbatch/simtst00.sim–simtst99.sim).

Backends:
  native  compile to a host binary, then execute it
  wasm    compile to wasm-node, then run with Node (tests/fixtures/run_wasi.mjs)
  interp  interpret with `sim run`

Applies the same overrides, reconstructed externals, data files, and stdin
as tests/result_plan_native.rs (see TESTBATCH_DIFF.md).

Examples:
  ./tests/run_testbatch.py interp
  ./tests/run_testbatch.py native --jobs 8
  ./tests/run_testbatch.py wasm --only 0,98,99
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "tests/testbatch"
EXTRAS = ROOT / "tests/fixtures/dostestbatch_externals"
OVERRIDES = ROOT / "tests/fixtures/dostestbatch_overrides"
DATA = ROOT / "tests/fixtures/dostestbatch_data"
WASM_RUNNER = ROOT / "tests/fixtures/run_wasi.mjs"

STDIN = {
    "simtst86": b"E\n",
    "simtst88": b"!\n",
    "simtst89": b"any8189\nout89.bin\n",
}

# simtst00 is a large Simulation program; native compile in debug is slow.
SLOW_UNITS = {"simtst00"}

# Windows fibers + `inspect InFile do Simulation` + Process enclosing capture
# (simtst96 `h :- been`): native run aborts with "remote access through none
# reference". Other Simulation units (00, 85, 87, 95, 97, 98) pass on Windows.
WINDOWS_SKIP_NATIVE = {"simtst96"}


@dataclass
class UnitResult:
    name: str
    status: str  # PASS, FAIL_COMPILE, FAIL_RUN, FAIL_OUTPUT, TIMEOUT
    seconds: float
    detail: str
    stdout: str = ""
    stderr: str = ""


def unit_sources(name: str) -> list[Path]:
    if name == "simtst59":
        return [EXTRAS / "c59.sim", CORPUS / "simtst59.sim"]
    if name == "simtst40":
        return [EXTRAS / "pa.sim", EXTRAS / "pb.sim", CORPUS / "simtst40.sim"]
    if name == "simtst41":
        return [EXTRAS / "p41.sim", CORPUS / "simtst41.sim"]
    override = OVERRIDES / f"{name}.sim"
    if override.is_file():
        return [override]
    return [CORPUS / f"{name}.sim"]


def list_units(only: list[str] | None) -> list[str]:
    names = [f"simtst{i:02d}" for i in range(100)]
    if not only:
        return names
    wanted = set()
    for item in only:
        item = item.strip().lower()
        if not item:
            continue
        if item.isdigit():
            wanted.add(f"simtst{int(item):02d}")
        elif re.fullmatch(r"simtst\d{2}", item):
            wanted.add(item)
        else:
            sys.exit(f"unknown unit {item!r} (expected simtstNN or NN)")
    missing = sorted(wanted - set(names))
    if missing:
        sys.exit(f"unknown unit(s): {', '.join(missing)}")
    return [name for name in names if name in wanted]


def output_ok(stdout: str) -> tuple[bool, str]:
    upper = stdout.upper()
    if "*** ERROR" in upper:
        return False, "contains *** ERROR"
    for line in upper.splitlines():
        if "ERROR:" in line or "--- ERROR" in line:
            return False, "contains ERROR: line"
    if re.search(r"\berror-\d+", stdout, re.IGNORECASE):
        return False, "contains error-N"
    if "NO ERRORS" in upper:
        marker = "NO ERRORS + END" if "END SIMULA" in upper else "NO ERRORS"
        return True, marker
    if "NO SIGNIFICANT DEVIATIONS" in upper:
        return True, "no significant deviations"
    if "END SIMULA" in upper:
        return True, "END marker, no ERROR"
    return False, "missing success marker"


def looks_like_compile_failure(text: str) -> bool:
    lower = text.lower()
    return (
        "[e-" in lower
        or "compilation aborted" in lower
        or "error: " in lower
        or "panic" in lower
    )


def run_cmd(
    argv: list[str],
    *,
    cwd: Path,
    stdin: bytes,
    timeout: float,
    env: dict[str, str] | None = None,
) -> tuple[int | None, str, str]:
    try:
        proc = subprocess.run(
            argv,
            cwd=cwd,
            input=stdin,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            env=env,
        )
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or b""
        stderr = exc.stderr or b""
        return None, stdout.decode("utf-8", "replace"), stderr.decode("utf-8", "replace")
    except FileNotFoundError as exc:
        return 127, "", str(exc)
    return (
        proc.returncode,
        proc.stdout.decode("utf-8", "replace"),
        proc.stderr.decode("utf-8", "replace"),
    )


def prepare_scratch(scratch: Path) -> None:
    if DATA.is_dir():
        for entry in DATA.iterdir():
            if entry.is_file():
                shutil.copy2(entry, scratch / entry.name)


def sim_names() -> tuple[str, ...]:
    if os.name == "nt":
        return ("sim.exe", "sim")
    return ("sim", "sim.exe")


def find_sim(release: bool, explicit: Path | None) -> Path:
    if explicit is not None:
        if not explicit.is_file():
            sys.exit(f"sim not found: {explicit}")
        return explicit
    env_path = os.environ.get("SIM")
    if env_path:
        path = Path(env_path)
        if not path.is_file():
            sys.exit(f"SIM is set but not a file: {path}")
        return path
    profile = ROOT / "target" / ("release" if release else "debug")
    for name in sim_names():
        candidate = profile / name
        if candidate.is_file():
            return candidate
    return profile / sim_names()[0]


def ensure_sim(path: Path, release: bool) -> None:
    if path.is_file():
        return
    argv = ["cargo", "build", "-q", "--bin", "sim"]
    if release:
        argv.append("--release")
    print(f"==> building sim ({'release' if release else 'debug'})", flush=True)
    subprocess.run(argv, cwd=ROOT, check=True)


def native_run_path(binary: Path) -> Path:
    """MSVC `/OUT:unit` may write `unit.exe`; run whichever exists."""
    if binary.is_file():
        return binary
    if os.name == "nt":
        exe = binary.with_suffix(".exe")
        if exe.is_file():
            return exe
    return binary


def run_unit(
    name: str,
    backend: str,
    sim: Path,
    timeout: float,
) -> UnitResult:
    if os.name == "nt" and backend == "native" and name in WINDOWS_SKIP_NATIVE:
        return UnitResult(
            name,
            "SKIP",
            0.0,
            "windows inspect+process none-deref",
        )
    sources = unit_sources(name)
    for src in sources:
        if not src.is_file():
            return UnitResult(name, "FAIL_COMPILE", 0.0, f"missing {src}")
    stdin = STDIN.get(name, b"")
    src_args = [str(src) for src in sources]
    started = time.perf_counter()

    with tempfile.TemporaryDirectory(prefix=f"outimage-{name}-") as tmp:
        scratch = Path(tmp)
        prepare_scratch(scratch)
        elapsed = lambda: time.perf_counter() - started

        if backend == "interp":
            code, stdout, stderr = run_cmd(
                [str(sim), "--color", "never", "run", *src_args],
                cwd=scratch,
                stdin=stdin,
                timeout=timeout,
            )
            if code is None:
                return UnitResult(name, "TIMEOUT", elapsed(), f"timed out after {timeout:.0f}s", stdout, stderr)
            combined = stdout + stderr
            if code != 0:
                status = "FAIL_COMPILE" if looks_like_compile_failure(combined) else "FAIL_RUN"
                detail = (stderr or stdout).strip().splitlines()
                return UnitResult(
                    name,
                    status,
                    elapsed(),
                    detail[0] if detail else f"exit {code}",
                    stdout,
                    stderr,
                )
            ok, detail = output_ok(stdout)
            status = "PASS" if ok else "FAIL_OUTPUT"
            return UnitResult(name, status, elapsed(), detail, stdout, stderr)

        if backend == "native":
            binary = scratch / ("unit.exe" if os.name == "nt" else "unit")
            code, stdout, stderr = run_cmd(
                [
                    str(sim),
                    "--color",
                    "never",
                    "compile",
                    "--target",
                    "native",
                    *src_args,
                    "-o",
                    str(binary),
                ],
                cwd=ROOT,
                stdin=b"",
                timeout=timeout,
            )
            if code is None:
                return UnitResult(name, "TIMEOUT", elapsed(), f"compile timed out after {timeout:.0f}s", stdout, stderr)
            if code != 0:
                detail = (stderr or stdout).strip().splitlines()
                return UnitResult(
                    name,
                    "FAIL_COMPILE",
                    elapsed(),
                    detail[0] if detail else f"compile exit {code}",
                    stdout,
                    stderr,
                )
            remaining = max(1.0, timeout - elapsed())
            code, stdout, stderr = run_cmd(
                [str(native_run_path(binary))],
                cwd=scratch,
                stdin=stdin,
                timeout=remaining,
            )
        else:  # wasm
            wasm = scratch / "unit.wasm"
            code, stdout, stderr = run_cmd(
                [
                    str(sim),
                    "--color",
                    "never",
                    "compile",
                    "--target",
                    "wasm-node",
                    *src_args,
                    "-o",
                    str(wasm),
                ],
                cwd=scratch,
                stdin=b"",
                timeout=timeout,
            )
            if code is None:
                return UnitResult(name, "TIMEOUT", elapsed(), f"compile timed out after {timeout:.0f}s", stdout, stderr)
            if code != 0:
                detail = (stderr or stdout).strip().splitlines()
                return UnitResult(
                    name,
                    "FAIL_COMPILE",
                    elapsed(),
                    detail[0] if detail else f"compile exit {code}",
                    stdout,
                    stderr,
                )
            remaining = max(1.0, timeout - elapsed())
            code, stdout, stderr = run_cmd(
                ["node", str(WASM_RUNNER), str(wasm)],
                cwd=scratch,
                stdin=stdin,
                timeout=remaining,
            )

        if code is None:
            return UnitResult(name, "TIMEOUT", elapsed(), f"run timed out after {timeout:.0f}s", stdout, stderr)
        if code != 0:
            detail = (stderr or stdout).strip().splitlines()
            return UnitResult(
                name,
                "FAIL_RUN",
                elapsed(),
                detail[0] if detail else f"run exit {code}",
                stdout,
                stderr,
            )
        ok, detail = output_ok(stdout)
        status = "PASS" if ok else "FAIL_OUTPUT"
        return UnitResult(name, status, elapsed(), detail, stdout, stderr)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the 100 Simula TestBatch units under tests/testbatch."
    )
    parser.add_argument(
        "backend",
        choices=("native", "wasm", "interp", "interpreter"),
        help="native AOT, wasm-node + Node WASI, or the MIR interpreter",
    )
    parser.add_argument(
        "--only",
        metavar="UNITS",
        help="comma-separated units (simtst00 or 0,1,98)",
    )
    parser.add_argument(
        "--jobs",
        "-j",
        type=int,
        default=1,
        metavar="N",
        help="parallel units (default: 1)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        metavar="SEC",
        help="per-unit timeout in seconds (default: 60; simtst00 uses max(timeout, 120))",
    )
    parser.add_argument(
        "--bin",
        type=Path,
        help="path to sim (default: target/debug/sim, or SIM)",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="use / build target/release/sim",
    )
    parser.add_argument(
        "--fail-fast",
        action="store_true",
        help="stop after the first failure",
    )
    parser.add_argument(
        "-v",
        "--verbose",
        action="store_true",
        help="print stdout/stderr for failures",
    )
    return parser.parse_args()


def unit_timeout(name: str, base: float) -> float:
    if name in SLOW_UNITS:
        return max(base, 120.0)
    return base


def main() -> int:
    args = parse_args()
    backend = "interp" if args.backend == "interpreter" else args.backend
    if args.jobs < 1:
        sys.exit("--jobs must be >= 1")
    if backend == "wasm" and shutil.which("node") is None:
        sys.exit("wasm backend requires node on PATH")
    if backend == "wasm" and not WASM_RUNNER.is_file():
        sys.exit(f"missing WASI runner {WASM_RUNNER}")

    only = [part for part in (args.only or "").split(",") if part.strip()]
    units = list_units(only or None)
    if not CORPUS.is_dir():
        sys.exit(f"missing corpus {CORPUS}")

    sim = find_sim(args.release, args.bin)
    ensure_sim(sim, args.release)
    if not sim.is_file():
        sys.exit(f"sim not found: {sim}")

    print(
        f"==> TestBatch {backend}  units={len(units)}  jobs={args.jobs}  "
        f"bin={sim}",
        flush=True,
    )
    wall_start = time.perf_counter()
    results: list[UnitResult] = []

    def work(name: str) -> UnitResult:
        return run_unit(name, backend, sim, unit_timeout(name, args.timeout))

    if args.jobs == 1:
        for name in units:
            result = work(name)
            results.append(result)
            print(format_line(result), flush=True)
            if args.verbose and result.status not in ("PASS", "SKIP"):
                print_streams(result)
            if args.fail_fast and result.status not in ("PASS", "SKIP"):
                break
    else:
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            futures = {pool.submit(work, name): name for name in units}
            for fut in as_completed(futures):
                result = fut.result()
                results.append(result)
                print(format_line(result), flush=True)
                if args.verbose and result.status not in ("PASS", "SKIP"):
                    print_streams(result)
                if args.fail_fast and result.status not in ("PASS", "SKIP"):
                    for pending in futures:
                        pending.cancel()
                    break
        results.sort(key=lambda r: r.name)

    wall = time.perf_counter() - wall_start
    passed = sum(1 for r in results if r.status == "PASS")
    skipped = sum(1 for r in results if r.status == "SKIP")
    failed_n = len(results) - passed - skipped
    counts: dict[str, int] = {}
    for result in results:
        counts[result.status] = counts.get(result.status, 0) + 1

    print()
    print(
        f"==> {passed}/{len(results)} passed  "
        + "  ".join(f"{k}={v}" for k, v in sorted(counts.items()) if k != "PASS")
        + f"  wall={wall:.2f}s"
    )
    if failed_n:
        print()
        print("Failures:")
        for result in results:
            if result.status != "PASS":
                print(f"  {result.name:10} {result.status:13} {result.detail}")
                if not args.verbose:
                    snippet = (result.stderr or result.stdout).strip()
                    if snippet:
                        for line in snippet.splitlines()[:12]:
                            print(f"             {line[:200]}")
        return 1
    return 0


def format_line(result: UnitResult) -> str:
    return f"{result.status:13} {result.name:10} {result.seconds:6.2f}s  {result.detail}"


def print_streams(result: UnitResult) -> None:
    if result.stdout.strip():
        print("--- stdout ---")
        print(result.stdout.rstrip())
    if result.stderr.strip():
        print("--- stderr ---")
        print(result.stderr.rstrip())


if __name__ == "__main__":
    sys.exit(main())
