import { readFileSync } from "node:fs";
import { argv } from "node:process";
import { instantiateSimulaWasm } from "./wasm_host.mjs";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run_js_utf8_greet.mjs <module.wasm>");
  process.exit(2);
}

const chunks = [];
const { instance } = await instantiateSimulaWasm(readFileSync(wasmPath), {
  console: {
    log(msg) {
      chunks.push(msg);
      chunks.push("\n");
    },
  },
  host: {
    greet(msg) {
      chunks.push(msg);
      chunks.push("\n");
    },
  },
});
if (typeof instance.exports._start !== "function") {
  throw new Error("wasm module missing _start export");
}
instance.exports._start();
process.stdout.write(chunks.join(""));
