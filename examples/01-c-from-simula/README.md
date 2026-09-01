# 01 — Simula calls C

A Simula **program** declares `external C procedure add` and links a C
object. This is design sketch A.

Expected stdout: `42`

```bash
cargo build   # once, from the repo root
./run.sh
```

`--link add.o` is the host linker, not `dlopen`. The thunk maps Simula
`integer` to `int64_t` with no conversion.
