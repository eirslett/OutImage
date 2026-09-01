// Instantiates a WasmGC module and calls its `probe` export, which returns 42
// when the host understands `struct.new` / `struct.get`.
//
// Exit codes: 0 = ran, 2 = bad usage, 3 = the host rejected the module. The
// Rust side (`tests/wasm_gc_probe.rs`) treats 3 as "no WasmGC here, skip"
// rather than a failure, so an old Node does not break the suite.
import { readFileSync } from "node:fs";
import { argv } from "node:process";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run_gc_probe.mjs <module.wasm>");
  process.exit(2);
}

try {
  const bytes = readFileSync(wasmPath);
  const module = await WebAssembly.compile(bytes);
  const instance = await WebAssembly.instantiate(module, {});
  process.stdout.write(`probe=${instance.exports.probe()}\n`);
} catch (error) {
  process.stdout.write(`unsupported=${error?.message ?? error}\n`);
  process.exit(3);
}
