import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { argv } from "node:process";
import { pathToFileURL } from "node:url";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run.mjs <module.wasm>");
  process.exit(2);
}

const { instantiateSimulaWasm } = await import(
  pathToFileURL(join(dirname(wasmPath), "wasm_host.mjs")).href
);
const { instance } = await instantiateSimulaWasm(readFileSync(wasmPath));
const result = instance.exports.add(40n, 2n);
process.stdout.write(`${result}\n`);
