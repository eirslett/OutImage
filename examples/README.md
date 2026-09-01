# Interop examples

Runnable sketches of Simula talking to other languages. They follow
[`docs/INTEROP.md`](../docs/INTEROP.md).

## Setup

```bash
cargo build
cd examples/01-c-from-simula
./run.sh
```

`run.sh` uses `target/debug/sim` (or `SIM=/path/to/sim`). Native
examples need a C compiler; wasm examples need Node; the browser example needs
a local HTTP server.

| # | Directory | Simula | Other language | Direction |
| --- | --- | --- | --- | --- |
| 1 | [01-c-from-simula](01-c-from-simula/) | program | C object | Simula → C |
| 2 | [02-simula-from-c](02-simula-from-c/) | library | C `main` | C → Simula |
| 3 | [03-c-host-embed](03-c-host-embed/) | library + `Host` | C embedding API | both |
| 4 | [04-libm](04-libm/) | program | libc `sqrt` | Simula → C |
| 5 | [05-js-from-simula](05-js-from-simula/) | wasm program | `console.log` | Simula → JS |
| 6 | [06-js-embeds-simula](06-js-embeds-simula/) | wasm module | Node | JS → Simula |
| 7 | [07-rust-host](07-rust-host/) | procedure module | Rust `Interpreter` | both |
| 8 | [08-simula-with](08-simula-with/) | program + lib | Simula | `--with` merge |
| 9 | [09-utf8-text](09-utf8-text/) | program | C, `--charset utf8` | Simula → C text |
| 10 | [10-ref-handles](10-ref-handles/) | program | C pin table | `ref` across GC |
| 11 | [11-python-ctypes](11-python-ctypes/) | library | Python | Python → Simula |
| 12 | [12-browser-canvas](12-browser-canvas/) | wasm module | browser JS | animation loop |

`./run-all.sh` from this directory smoke-tests the non-browser examples
(skipping those whose tools are missing).
