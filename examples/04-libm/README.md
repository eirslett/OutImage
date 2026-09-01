# 04 — Simula calls libm

Identification `libm:sqrt` asks the **host linker** for `-lm` (skipped on
Darwin, where libm is in libSystem). This is not `dlopen`.

ENVIRONMENT `sqrt` is a compiler builtin. This example binds libc’s `sqrt`
under a different Simula name so the two cannot be confused.

Expected stdout: `5.0` (approximately, via `OutFix`).
