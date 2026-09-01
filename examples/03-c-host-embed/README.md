# 03 — C embedding API (Host table)

The same Simula source can import a portable `Host` procedure. A C program
loads the library with `simrt_instantiate`, provides `add`, then
`simrt_call`s `sim_combo`. Include [`runtime/embed.h`](../../runtime/embed.h).

Expected stdout:

```
42
0.0
0
```

(`simrt_sim_now` / `simrt_sim_step` are idle here; a Simulation host
would drive them from the event loop.)
