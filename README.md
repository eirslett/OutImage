# OutImage

A collection of tools for the [Simula](https://en.wikipedia.org/wiki/Simula) programming language, written in Rust. The CLI is **`sim`**.

## Prerequisites

### Using a prebuilt `sim` binary

No Rust toolchain is required. The Simula C runtime is embedded in the compiler;
native linking uses the **host** linker (override with `SIM_LINKER`).

| Host | Native AOT | Notes |
| --- | --- | --- |
| **macOS** | Yes | Needs Xcode CLT (`ld` + SDK via `xcrun`); `-g` writes a `.dSYM` |
| **Linux** | Yes | Needs `cc`/`clang`/`gcc` (or `ld`) on PATH; `-g` keeps DWARF in the ELF |
| **Windows** | Yes | Needs MSVC `link.exe` + `LIB` (Developer Command Prompt / Build Tools); `-g` keeps DWARF in the PE |
| **WebAssembly** | Emit yes | `wasm-node` / `wasm-browser`; run with Node or a browser |

Cross-OS native linking (`--target linux-*` on macOS, etc.) is **not** supported yet:
the bundled C runtime matches the **host**. Prefer wasm or a matching host OS.

### Building sim from source

- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain; edition 2024)
- A C compiler for the build script (`cc`, `clang`, or MSVC on Windows)

Check your installation:

```bash
rustc --version
cargo --version
```

## Building

Build the compiler in debug mode:

```bash
cargo build
```

Build an optimized release binary:

```bash
cargo build --release
```

The `sim` executable is written to `target/debug/sim` or
`target/release/sim` (on Windows: `simula.exe`).

## Running

Interpret and run a Simula program via the **MIR interpreter** (default
semantics oracle for `sim run`):

```bash
cargo run -- run path/to/program.sim
# or, with an installed binary:
sim run path/to/program.sim
```

`sim run` always uses the MIR interpreter.

Multiple source files are concatenated in order (joined with a newline). Diagnostics still point at the originating file:

```bash
sim run a.sim b.sim
sim compile prelude.sim main.sim -o prog
```

### Debug adapter

`sim dap` / `sim debug` execute the same MIR interpreter as `run`
(breakpoints, stepping, locals, Simulation SQS).

### Native and WebAssembly compilation

Compile to a native executable (Cranelift + host linker + bundled runtime):

```bash
sim compile hello.sim --target native -o hello
./hello
```

Cross-compile for other platforms:

```bash
# Same-OS arch variants may work; cross-OS native linking is not supported.
sim compile hello.sim --target linux-x86_64 -o hello   # on a Linux host
sim compile hello.sim --target windows-x86_64 -o hello.exe  # experimental — not linked yet
sim compile hello.sim --target wasm-node -o hello.wasm      # then: node hello.mjs
sim compile hello.sim --target wasm-browser -o hello.wasm   # then: open hello.html
```

Each wasm compile also writes a runner beside the module: **`hello.mjs`** (`node hello.mjs`) or **`hello.html`** + **`hello.js`** (open the HTML via a local server). You do not instantiate the `.wasm` yourself.

That last target compiles a **Simula program** to WasmGC.

The **browser playground** (`cargo browser-interp`) compiles the interpreter
itself for the page in [`website/`](website/). See
[`website/README.md`](website/README.md).

### Interop (C, JS, Rust, Python)

Simula programs can call out (`external C/JS/Host procedure`) and hosts can
call in (`--crate-type lib` on native, wasm exports). Tutorial:
[`docs/INTEROP.md`](docs/INTEROP.md). Runnable sketches: [`examples/`](examples/).

List all supported targets:

```bash
sim targets
```

Native linking embeds only the small C runtime archive in `sim`. The host
linker is resolved at compile time (Apple `ld`, `cc`/`clang`/`gcc`, or MSVC
`link.exe`). End users do not need Rust or `rust-lld`.

### Debugging

Prefer the VS Code / Cursor interpreter debugger (`sim dap`) or
`sim debug` — see [`docs/DEBUG.md`](docs/DEBUG.md).

### Debugging native binaries (lldb / gdb)

Compile with `-g` for DWARF. On **macOS**, Darwin strips DWARF into a companion
`.dSYM`; on **Linux**, DWARF stays in the ELF.

**macOS (lldb + dSYM):**

```bash
sim compile -g prog.sim -o prog
lldb ./prog
(lldb) b /absolute/path/to/prog.sim:3
(lldb) run
```

**Linux (gdb or lldb):**

```bash
sim compile -g prog.sim -o prog
gdb ./prog
(gdb) break prog.sim:3
(gdb) run
```

Breakpoints use Simula source paths and line numbers. A JSON side-map
`<output>.sim-map` is also written for tooling that prefers MIR spans.

Inspect intermediate artifacts:

```bash
sim ast prog.sim                 # annotated AST with spans
sim mir prog.sim                 # print MIR dump
sim compile --emit-mir prog.sim  # also write `<output>.mir`
sim compile --emit-obj prog.sim  # write `prog.o` (skip linking)
sim compile --emit-asm prog.sim  # also write `<output>.s` (Cranelift disasm)
```

### Language server

Start the LSP server on stdin/stdout (for editor integration):

```bash
sim lsp
```

Diagnostics, hover, completion, rename, format, and more — see [`docs/LSP.md`](docs/LSP.md).
**VS Code / Cursor:** extension in [`editors/vscode`](editors/vscode/README.md).

| Target | Triple | Backend | Output |
| --- | --- | --- | --- |
| `native` | host default | Cranelift | executable |
| `linux-x86_64` | x86_64-unknown-linux-gnu | Cranelift | executable |
| `linux-aarch64` | aarch64-unknown-linux-gnu | Cranelift | executable |
| `macos-x86_64` | x86_64-apple-darwin | Cranelift | executable |
| `macos-aarch64` | aarch64-apple-darwin | Cranelift | executable |
| `windows-x86_64` | x86_64-pc-windows-msvc | Cranelift | `.exe` |
| `wasm-node` | wasm32-wasi | wasm-encoder | `.wasm` (Node.js + WASI) |
| `wasm-browser` | wasm32-unknown-unknown | wasm-encoder | `.wasm` (browser; live MIR via `env.fd_write`) |

On macOS, native linking still requires Xcode command line tools for the platform SDK (`xcrun --show-sdk-path`).

## Test-driven development

Development follows a red-green-refactor cycle:

1. **Red** — add a failing test that describes the next behavior
2. **Green** — implement the smallest change that makes it pass
3. **Refactor** — clean up while keeping tests green

### Where tests live

| Kind | Location | Purpose |
| --- | --- | --- |
| Unit tests | `src/<module>.rs` under `#[cfg(test)]` | One compiler phase at a time (lex, parse, …) |
| Integration tests | `tests/*.rs` | End-to-end `compile()` behavior |
| Fixtures | `tests/fixtures/*.sim` | Reusable Simula source snippets |

Shared helpers for integration tests are in `tests/common/mod.rs`.

### Useful test commands

Run all tests (ignored tests are skipped by default):

```bash
cargo test
```

Run only the tests for one module:

```bash
cargo test lex::
cargo test parse::
```

Run a single test by name:

```bash
cargo test tokenizes_begin_keyword
```

Run ignored tests — the backlog of not-yet-implemented behavior:

```bash
cargo test -- --ignored
```

Run ignored tests for one crate only:

```bash
cargo test --test lex -- --ignored
```

Watch tests while developing (requires [cargo-watch](https://github.com/watchexec/cargo-watch)):

```bash
cargo watch -x test
```

### Workflow for a new feature

1. Add an `#[ignore]` test (or a failing test) in the relevant module
2. Remove `#[ignore]` (or run `cargo test -- --ignored`) to confirm it fails
3. Implement the feature until the test passes
4. Run the full suite: `cargo test`
5. Add fixture-based integration tests when behavior spans multiple phases

## Dependencies

| Crate | Role |
| --- | --- |
| [clap](https://github.com/clap-rs/clap) | CLI argument parsing |
| [cranelift](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift) | Native code generation |
| [ariadne](https://codeberg.org/zesterer/ariadne) | Source-aware diagnostics (labels, notes/helps, `--color`, `NO_COLOR`) |
| [target-lexicon](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift/target-lexicon) | Target triple parsing |
| [thiserror](https://github.com/dtolnay/thiserror) | Structured compiler errors |
| [wasm-encoder](https://github.com/bytecodealliance/wasm-tools) | WebAssembly output |

Dev dependencies: [insta](https://github.com/mitsuhiko/insta) (snapshot tests).

The lexer and parser use a hand-written Simula-aware lexer (`lex/lexer.rs`, `lex/special.rs`, plus `lex/number.rs`, `lex/string.rs`, and `lex/character.rs`) and combinator-based parsers in `parse/` (built on [chumsky](https://github.com/zebulon859/chumsky)) behind stable `TokenKind` / `TokenStream` / AST interfaces.

## Development

Type-check without producing a binary:

```bash
cargo check
```

Run tests:

```bash
cargo test
```

Run tests with output from passing tests:

```bash
cargo test -- --nocapture
```

Format code:

```bash
cargo fmt
```

Run the linter:

```bash
cargo clippy
```

## Standard library

OutImage ships a standard library in two layers:

| Layer | Location | Role |
| --- | --- | --- |
| Simula API | `stdlib/*.sim` | Classes and `external` procedure declarations exposed to Simula code |
| Runtime | `src/runtime/` | Rust implementations of those `external` bindings |

ENVIRONMENT and filesystem builtins are recognized in semantic analysis and
MIR lowering. `stdlib/*.sim` is the documented Simula-facing surface (and
the LSP go-to-definition target for ENVIRONMENT names); generated native
code links against the bundled C runtime.

### Filesystem module

`stdlib/filesystem.sim` defines the Simula-facing API:

- `open`, `read`, `write`, `close` — file handles
- `exists` — check whether a path exists
- `list_dir` — list directory entries

The Rust implementation lives in `src/runtime/fs.rs` and is tested independently of the compiler pipeline.

Run runtime tests:

```bash
cargo test --test runtime
```

Run stdlib registry tests:

```bash
cargo test --test stdlib
```

## Project layout

```
src/
├── lib.rs        # compiler library
├── main.rs       # CLI (compile / run / targets)
├── source.rs     # SourceFile + CompositeSource (multi-file concat / origin map)
├── target/       # cross-compilation target presets
├── driver/       # backend dispatch (interpreter vs Cranelift)
├── ast.rs        # abstract syntax tree
├── lex/          # lexer
├── parse/        # parser
├── semantic.rs   # semantic analysis
├── codegen/      # interpreter + Cranelift/wasm backends, linker
├── error.rs      # compile-time error types
├── stdlib/       # stdlib module registry
└── runtime/      # Rust runtime (interpreter)
sim-rt/       # (removed — runtime lives in runtime/runtime.c, bundled at build time)
runtime/          # C runtime linked into native executables
stdlib/
tests/
examples/         # interop sketches (C, JS, Rust, Python, Simula --with)
docs/             # LSP, DAP, runtime, interop tutorial
```

The `run` subcommand uses the interpreter backend. The `compile` subcommand uses Cranelift for native targets and `wasm-encoder` for WebAssembly.
