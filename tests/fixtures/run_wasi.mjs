import { readFileSync } from "node:fs";
import { WASI } from "node:wasi";
import { argv, env } from "node:process";
import { instantiateSimulaWasm } from "./wasm_host.mjs";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run_wasi.mjs <module.wasm>");
  process.exit(2);
}

// Node's built-in WASI already implements `wasi_snapshot_preview1.fd_read`
// against the real `stdin` fd (default fd 0), so a `CallInLine` MIR program
// reads actual process stdin here with no extra polyfill.
const wasi = new WASI({
  version: "preview1",
  args: argv.slice(1),
  env,
  returnOnExit: true,
});

const { instance } = await instantiateSimulaWasm(
  readFileSync(wasmPath),
  { ...wasi.getImportObject() },
  {
    stdout: (text) => process.stdout.write(text),
    stderr: (text) => process.stderr.write(text),
  },
);
wasi.start(instance);
