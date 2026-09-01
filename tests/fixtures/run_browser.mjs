import { readFileSync } from "node:fs";
import { argv } from "node:process";
import { createEnvImports, instantiateSimulaWasm } from "./wasm_host.mjs";

const wasmPath = argv[2];
if (!wasmPath) {
  console.error("usage: node run_browser.mjs <module.wasm>");
  process.exit(2);
}

const bytes = readFileSync(wasmPath);
const module = await WebAssembly.compile(bytes);

/** @type {WebAssembly.Memory | null} */
let memory = null;
const chunks = [];
const host = createEnvImports(() => memory, (text) => chunks.push(text));

/** Minimal WASI fd_write polyfill for wasm-browser (same ABI as node). */
function fdWrite(_fd, iovs, iovsLen, nwritten) {
  const v = new DataView(memory.buffer);
  let written = 0;
  for (let i = 0; i < iovsLen; i++) {
    const base = iovs + i * 8;
    const ptr = v.getUint32(base, true);
    const len = v.getUint32(base + 4, true);
    if (len > 0) {
      const bytes = new Uint8Array(memory.buffer, ptr, len);
      chunks.push(String.fromCharCode(...bytes));
      written += len;
    }
  }
  v.setUint32(nwritten, written, true);
  return 0;
}

// Lazily slurped on the first `fd_read` call so programs that never read
// stdin (almost all of them) never touch fd 0 / risk blocking on it.
let stdinBuffer = null;
let stdinPos = 0;
function stdinBytes() {
  if (stdinBuffer === null) {
    try {
      stdinBuffer = readFileSync(0);
    } catch {
      stdinBuffer = Buffer.alloc(0);
    }
  }
  return stdinBuffer;
}

/**
 * Minimal WASI fd_read polyfill for wasm-browser (same ABI as node):
 * fills `iovs` in order from a stdin buffer read once on first use, and
 * reports the total bytes copied via `nread`. Only `fd === 0` is supported.
 */
function fdRead(_fd, iovs, iovsLen, nread) {
  const buf = stdinBytes();
  const v = new DataView(memory.buffer);
  let readTotal = 0;
  for (let i = 0; i < iovsLen; i++) {
    const base = iovs + i * 8;
    const ptr = v.getUint32(base, true);
    const len = v.getUint32(base + 4, true);
    const remaining = buf.length - stdinPos;
    const n = Math.max(0, Math.min(len, remaining));
    if (n > 0) {
      new Uint8Array(memory.buffer, ptr, n).set(buf.subarray(stdinPos, stdinPos + n));
      stdinPos += n;
      readTotal += n;
    }
    if (n < len) break; // buffer exhausted
  }
  v.setUint32(nread, readTotal, true);
  return 0;
}

const diskStub = host.diskBasicioStub;

const { instance, memory: mem } = await instantiateSimulaWasm(module, {
  env: {
    ...host,
    fd_write: fdWrite,
    fd_read: fdRead,
    error: () => {
      throw new WebAssembly.RuntimeError("error: not available in browser");
    },
    basicio_register: diskStub("basicio_register"),
    basicio_open: diskStub("basicio_open"),
    basicio_close: diskStub("basicio_close"),
    basicio_isopen: diskStub("basicio_isopen"),
    basicio_out_text: diskStub("basicio_out_text"),
    basicio_out_char: diskStub("basicio_out_char"),
    basicio_out_image: diskStub("basicio_out_image"),
    basicio_break_out_image: diskStub("basicio_break_out_image"),
    basicio_in_image: diskStub("basicio_in_image"),
    basicio_in_char: diskStub("basicio_in_char"),
    basicio_endfile: diskStub("basicio_endfile"),
    basicio_image: diskStub("basicio_image"),
    basicio_set_image: diskStub("basicio_set_image"),
    basicio_pos: diskStub("basicio_pos"),
    basicio_length: diskStub("basicio_length"),
    basicio_setpos: diskStub("basicio_setpos"),
    basicio_line: diskStub("basicio_line"),
    basicio_filename: diskStub("basicio_filename"),
    basicio_lastitem: diskStub("basicio_lastitem"),
    basicio_inint: diskStub("basicio_inint"),
    basicio_inreal: diskStub("basicio_inreal"),
    basicio_infrac: diskStub("basicio_infrac"),
    basicio_intext: diskStub("basicio_intext"),
    basicio_out_real: diskStub("basicio_out_real"),
    basicio_out_fix: diskStub("basicio_out_fix"),
    basicio_out_frac: diskStub("basicio_out_frac"),
    basicio_out_int: diskStub("basicio_out_int"),
    basicio_open_byte: diskStub("basicio_open_byte"),
    basicio_in_byte: diskStub("basicio_in_byte"),
    basicio_out_byte: diskStub("basicio_out_byte"),
    basicio_locate: diskStub("basicio_locate"),
    basicio_location: diskStub("basicio_location"),
    basicio_lastloc: diskStub("basicio_lastloc"),
    basicio_setaccess: diskStub("basicio_setaccess"),
    basicio_eject: diskStub("basicio_eject"),
    basicio_linesperpage: diskStub("basicio_linesperpage"),
    basicio_inrecord: diskStub("basicio_inrecord"),
  },
});
memory = mem;
if (typeof instance.exports._start !== "function") {
  throw new Error("wasm-browser module missing _start export");
}
instance.exports._start();
process.stdout.write(chunks.join(""));
