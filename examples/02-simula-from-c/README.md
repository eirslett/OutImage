# 02 — C calls Simula

Compile a **procedure module** as `--crate-type lib`. A C `main` links the
shared library and calls `sim_add`. Native library exports use a `sim_`
prefix so they cannot collide with `simrt_*`.

Expected stdout: `42`

```bash
./run.sh
```
