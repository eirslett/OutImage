import { readFileSync } from "node:fs";
import { argv } from "node:process";
import { instantiateSimulaWasm } from "./wasm_host.mjs";

const wasmPath = argv[2];
const exportName = argv[3] || "add";
if (!wasmPath) {
  console.error("usage: node run_export.mjs <module.wasm> [export]");
  process.exit(2);
}

const { instance } = await instantiateSimulaWasm(readFileSync(wasmPath));
if (typeof instance.exports._start !== "function") {
  throw new Error(`module missing _start: ${Object.keys(instance.exports)}`);
}
if (typeof instance.exports.step !== "function") {
  throw new Error(`module missing step: ${Object.keys(instance.exports)}`);
}
const fn = instance.exports[exportName];
if (typeof fn !== "function") {
  throw new Error(`missing export ${exportName}: ${Object.keys(instance.exports)}`);
}
const result = fn(40n, 2n);
process.stdout.write(`${result}\n`);
