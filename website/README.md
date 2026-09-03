# OutImage playground

In-browser demo: the **OutImage MIR interpreter** compiled to WebAssembly.
Edit a Simula program, hit Run, and use the terminal for stdin / stdout /
stderr.

This is **not** `sim compile --target wasm-browser`. The compiler itself
runs in the page.

## Prerequisites

- Rust stable (workspace toolchain)
- `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`)
- Node.js 20+

## Build the interpreter wasm

The website does not compile Rust. From the repo root:

```bash
cargo browser-interp
```

That is a Cargo alias (see [`.cargo/config.toml`](../.cargo/config.toml)): it
builds the [`outimage-browser-interp`](../browser-interp) crate for
`wasm32-unknown-unknown` and writes wasm-bindgen glue to
`target/outimage-browser-interp/`. Vite aliases that directory as
`outimage-browser-interp`.

## Dev

```bash
cd website
npm install
npm run dev
```

Vite starts only; it fails fast if `target/outimage-browser-interp/` is missing.
The worker calls `init()` explicitly (no `vite-plugin-wasm`).

## Production site

```bash
cargo browser-interp
cd website
npm run build
```

CI deploys `website/dist` to [GitHub Pages](https://eirslett.github.io/OutImage/)
on every push to `main`, after the Linux / macOS / Windows test matrix passes.

## Limits

- Interpreter only (no native/wasm AOT of the Simula program)
- No disk files (`fileRead` / DirectFile)
- TextMate highlighting uses
  [`editors/vscode/syntaxes/simula.tmLanguage.json`](../editors/vscode/syntaxes/simula.tmLanguage.json)
