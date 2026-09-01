# Supported Simula subset

This document summarizes what *sim* implements today versus what remains
open.

## What works where (hosts)

| Capability | macOS | Linux | Windows |
| --- | --- | --- | --- |
| Interpreter / `run` / `check` / LSP / DAP | Yes | Yes | Yes* |
| Native AOT (`compile --target native`) | Yes | Yes | Yes* (MSVC `LIB` required) |
| Native `-g` | DWARF + `.dSYM` | DWARF in ELF | DWARF in PE (CodeLLDB; PDB later) |
| Wasm AOT (`wasm-node` / `wasm-browser`) | Yes | Yes | Yes* |
| Cross-OS native link | No (host runtime only) | No | No |

\*Requires a successful `cargo build` on that host (C compiler for the runtime).
Native AOT also needs the host linker: Xcode CLT (`ld`) on macOS, `cc`/`clang`/`gcc`
on Linux, MSVC `link.exe` + `LIB` on Windows. CI: Linux + macOS full suite;
Windows host job covers lib + native compile / DAP / CLI.

Details: [`docs/DEBUG.md`](DEBUG.md).

## Implemented (basic)

| Area | Notes |
| --- | --- |
| Lex / parse | Core Standard syntax, directives, end-comments, named `end`, `--` line comments |
| Expressions | Arithmetic, boolean, text concat, `is`/`in`, `qua`/`this`/`new` |
| Control flow | `if` / `while` / `for` / `goto` (incl. outside procedures, switch designators) |
| Procedures | Value / name / reference; formal procedures; mixed name+text/`ref`/array ref (interp + MIR inline) |
| Classes | Prefix chains, virtuals, protection, inspect connection blocks, prefixed blocks; enclosing I64/Bool/F64/Text/ObjectRef snapshot at `new`; concatenation identifier substitution (§5.5.2.6–2.7 / §5.5.6.6); external builtin aliases (§6.3.7) |
| Text | Frames, edit/deedit, decimalmark/lowten; content + reference + ranking relations (interp + MIR) |
| ENVIRONMENT | Interpreter + native MIR for Ch.9 (CURRENT* attrs, accurate sourceline, ArrayF64 bounds/histo/discrete/linear/erlang/histd, datetime/cputime/clocktime); Wasm: subset without random/array ENV helpers; `stdlib/environment.sim` |
| BASICIO | Chapter 10: Standard file APIs; item I/O; PrintFile; DirectFile + bytefiles; native image/byte/DirectFile + `terminate_program`; free SYSIN/SYSOUT embedding (`inspect` semantics); wasm terminal-only |
| Simulation | Interpreter SQS/SIMSET MVP; MIR native+wasm hold/passivate/cancel/current/wait + timed activate + SIMSET + enclosing `wait(q)`; detach in if/else / mid-branch / multi-detach (interp + MIR); while (interp + MIR) |
| Backends | MIR → native (Cranelift, mark-sweep GC); MIR → wasm (host WasmGC + Simulation/SQS/SIMSET; whole-file I/O terminal-only; ENV subset) |
| Memory / GC | Interp + native mark-sweep; native precise frame roots; wasm host WasmGC only |
| Diagnostics | Ariadne (`E-lex` / `E-parse` / `E-semantic` / `E-codegen`) |
| CI | Ubuntu + macOS matrix; Windows host (native PE + DAP/CLI); fmt/clippy/deny on Ubuntu |
| Host portability | Native AOT uses the host C runtime and linker; wasm is the portable AOT path |

## Still open (high level)

- Remaining ENV MIR gaps on wasm (random / some array ENV helpers); release packages
- Nested detach/resume into labelled if/loop sites
- `sim test` / conformance dashboard
- MIR for-detach while-elements; ByteFile/DirectFile on wasm
- Richer multi-stack debugger UX beyond `__simrt_coro_pc`
- LSP polish (inlay hints, auto-import, fuzz/soak, marketplace)

GC is **done**: interpreter mark-sweep; native precise frame roots (heap
payloads still conservative by design); wasm → host WasmGC only (refs-only).
DosTest 100/100 on all three backends.

## Tooling

| Tool | Notes |
| --- | --- |
| `sim check` | Front-end + MIR lower |
| `sim lsp` | Full LSP: sync, diagnostics (push+pull), nav, complete, hierarchy, workspace index — see [`docs/LSP.md`](LSP.md) |
| `sim explain` / `--json` | Diagnostic UX |

See the **Still open** list above for remaining work.
