# 06 — JavaScript calls Simula

A wasm module always exports `_start` plus public procedures. Node
instantiates the module and calls the exported `add` (raw name on wasm);
it does not need to run `_start`. Integers at the wasm edge are `i64`, so
the host passes BigInt. Linear memory stays inside `instantiateSimulaWasm`.

Expected stdout: `42`

```js
const { instance } = await instantiateSimulaWasm(readFileSync(wasmPath));
const result = instance.exports.add(40n, 2n);
```
