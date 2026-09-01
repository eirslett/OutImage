# 10 — Opaque `ref` handles

C does not see object layout. A `ref (Cell)` at the boundary is an opaque
pointer. The host **pins** it so GC cannot collect the object while C holds
it, then returns the same handle.

Expected stdout: `7`

`runtime/gc.h` declares `simrt_ref_pin` / `unpin` / `get`. Interpreter
hosts use `ctx.root(value)` instead.
