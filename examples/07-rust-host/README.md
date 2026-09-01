# 07 — Rust interpreter host

The MIR interpreter is the semantics oracle and the in-process embedding
API. Same Simula as a JS canvas host would use: `Host draw` plus exported
`hypot` / `tick`.

```bash
cargo run --manifest-path Cargo.toml
```

`sim` is a path dependency with default features off (no Cranelift).
Expected stdout includes `hypot(3, 4) = 5` and a `plot` line.

ENVIRONMENT already has `draw` (a random-number procedure). Host plotting
uses the name `plot`.
