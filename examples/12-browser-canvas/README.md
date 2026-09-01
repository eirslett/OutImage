# 12 — Browser canvas (JS event loop + Simula model)

Design sketch B. A wasm module exports `tick(t)`; Simula calls
`Host plot(x, y)`. The page owns `requestAnimationFrame` and does not run
`_start`. (`draw` is an ENVIRONMENT random procedure, so the host hook is
named `plot`.) The model is a rhodonea / rose curve (`k` in `model.sim`:
even `k` → `2*k` petals). Linear memory stays inside `instantiateSimulaWasm`;
the host only implements `plot`.

`run.sh` compiles `model.wasm` and `wasm_host.mjs` here. Serve **this
directory** (the helper is next to the module, not under `tests/fixtures`):

```bash
./run.sh
python3 -m http.server 8765
# open http://127.0.0.1:8765/
```
