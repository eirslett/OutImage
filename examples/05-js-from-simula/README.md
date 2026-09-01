# 05 — Simula calls JavaScript

Compile a **program** for `wasm-browser`. `external JS procedure greet =
"console.log"` becomes `import "console" "log"`. The argument is a JavaScript
`string`; `instantiateSimulaWasm` owns linear memory and the copy out of the
Simula text frame.

Expected stdout: `hello from Simula`

```bash
./run.sh
```

Needs Node. The runner is:

```js
const { instance } = await instantiateSimulaWasm(readFileSync(wasmPath), {
  console,
});
instance.exports._start();
```
