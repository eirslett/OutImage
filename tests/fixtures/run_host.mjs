import { readFileSync } from "node:fs";
import { argv } from "node:process";
import { instantiateSimulaWasm } from "./wasm_host.mjs";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run_host.mjs <module.wasm>");
  process.exit(2);
}

const chunks = [];
const { instance } = await instantiateSimulaWasm(
  readFileSync(wasmPath),
  {
    host: {
      add(a, b) {
        return a + b;
      },
      onTick(_now) {},
      keep(obj) {
        return obj;
      },
    },
  },
  { stdout: (text) => chunks.push(text) },
);
if (typeof instance.exports._start !== "function") {
  throw new Error("wasm module missing _start export");
}
instance.exports._start();
process.stdout.write(chunks.join(""));
