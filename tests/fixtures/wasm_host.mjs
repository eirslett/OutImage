/** Shared host helpers for sim wasm-node / wasm-browser runners. */

async function loadNodeFs() {
  if (typeof process === "undefined" || !process.versions?.node) {
    return null;
  }
  const { createRequire } = await import("node:module");
  return createRequire(import.meta.url)("node:fs");
}

const nodeFs = await loadNodeFs();

function fs() {
  if (!nodeFs) {
    throw new WebAssembly.RuntimeError("disk BASICIO requires Node.js");
  }
  return nodeFs;
}

// Fixed data layout shared with `src/codegen/wasm.rs`. The SysOut image header
// is a text frame (`ptr`, `len`, `pos`, `pad`, `start`, `main_len`) followed by
// the page line counter and the character buffer.
const SYSOUT_BASE_PTR = 32;
const IMAGE_OFF_LEN = 4;
const IMAGE_OFF_POS = 8;
const IMAGE_OFF_MAIN_LEN = 20;
const IMAGE_OFF_LINE = 24;
const IMAGE_OFF_BUF = 32;
const IMAGE_BUF_SIZE = 4096;
const HEAP_CURSOR = 4;
const FRAME_SIZE = 24;
const FRAME_OFF_PTR = 0;
const FRAME_OFF_LEN = 4;
const FRAME_OFF_POS = 8;
const FRAME_OFF_PAD = 12;
const FRAME_OFF_START = 16;
const FRAME_OFF_MAIN_LEN = 20;
const EM_CHAR = "\x19";

const RT_ENV_EXPORTS = [
  "f64_pow",
  "text_getint",
  "text_putint",
  "text_getfrac",
  "text_putfrac",
  "text_getreal",
  "text_putfix",
  "text_putreal",
  "out_real",
  "out_fix",
  "out_frac",
  "ln",
  "exp",
  "sin",
  "cos",
  "arctan",
  "addepsilon",
  "subepsilon",
  "randint",
  "uniform",
  "negexp",
  "normal",
  "draw",
];

function ffiCharsetUtf8(flag) {
  return Number(flag) !== 0;
}

/** Linear-scratch → JS string. Application imports never call this. */
function decodeFfiText(memory, ptr, len, utf8) {
  const n = Number(len);
  if (!memory || n <= 0) return "";
  const bytes = new Uint8Array(memory.buffer, Number(ptr), n);
  if (ffiCharsetUtf8(utf8)) {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  }
  return new TextDecoder("latin1").decode(bytes);
}

/** JS string → linear scratch `(ptr, len)` for the wasm thunk to copy in. */
function encodeFfiText(memory, value, utf8) {
  if (!memory) {
    throw new WebAssembly.RuntimeError("wasm memory not ready");
  }
  const s = value == null ? "" : typeof value === "string" ? value : String(value);
  if (s.length === 0) return [0, 0];
  let bytes;
  if (ffiCharsetUtf8(utf8)) {
    for (const ch of s) {
      if (ch.codePointAt(0) > 255) {
        throw new WebAssembly.RuntimeError(
          "utf8 FFI text contains a code point above 255",
        );
      }
    }
    bytes = new TextEncoder().encode(s);
  } else {
    bytes = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c > 255) {
        throw new WebAssembly.RuntimeError(
          "latin1 FFI text contains a code point above 255",
        );
      }
      bytes[i] = c;
    }
  }
  const view = new DataView(memory.buffer);
  const ptr = view.getUint32(HEAP_CURSOR, true);
  view.setUint32(HEAP_CURSOR, ptr + bytes.length, true);
  new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);
  return [ptr, bytes.length];
}

function defaultStdout(text) {
  if (typeof process !== "undefined" && process.stdout?.write) {
    process.stdout.write(text);
  }
}

/**
 * Compile a sim wasm module, instantiate the bundled `simrt` helper
 * against its exported memory, then instantiate the program.
 *
 * Fills `env` (BASICIO, `fd_write` / `fd_read`, runtime math) and the
 * `sim` JS-string helpers. Application code passes `console` / `host` /
 * `js` and never mentions `Memory`; linear scratch stays inside this helper.
 *
 * @param {BufferSource | WebAssembly.Module} bytes
 * @param {WebAssembly.Imports} [importObject]
 * @param {{ stdout?: (text: string) => void, stderr?: (text: string) => void }} [options]
 */
export async function instantiateSimulaWasm(bytes, importObject = {}, options = {}) {
  const module =
    bytes instanceof WebAssembly.Module ? bytes : await WebAssembly.compile(bytes);
  const sections = WebAssembly.Module.customSections(module, "simrt");
  if (!sections.length) {
    throw new Error("wasm module is missing the simrt runtime section");
  }
  const rtBytes = sections[0];

  let rtExports = null;
  const thunk = (name) => (...args) => {
    if (!rtExports) {
      throw new WebAssembly.RuntimeError(`runtime ${name} used before init`);
    }
    const fn = rtExports[`simrt_${name}`];
    if (typeof fn !== "function") {
      throw new WebAssembly.RuntimeError(`runtime missing simrt_${name}`);
    }
    return fn(...args);
  };

  /** @type {WebAssembly.Memory | null} */
  let memory = null;
  const envHost = createEnvImports(
    () => memory,
    options.stdout ?? defaultStdout,
    options.stderr,
  );
  const env = { ...envHost, ...(importObject.env || {}) };
  const envNames = new Set(
    WebAssembly.Module.imports(module)
      .filter((imp) => imp.module === "env")
      .map((imp) => imp.name),
  );
  for (const name of RT_ENV_EXPORTS) {
    if (envNames.has(name)) {
      env[name] = thunk(name);
    }
  }

  const sim = {
    ...(importObject.sim || {}),
    text_from_bytes(ptr, len, utf8) {
      return decodeFfiText(memory, ptr, len, utf8);
    },
    bytes_from_text(value, utf8) {
      return encodeFfiText(memory, value, utf8);
    },
  };
  const imports = { ...importObject, env, sim };

  const instance = await WebAssembly.instantiate(module, imports);
  memory = instance.exports.memory;
  if (!memory) {
    throw new Error("wasm module did not export memory");
  }
  const rtMinPages = 32;
  const havePages = Math.floor(memory.buffer.byteLength / 65536);
  if (havePages < rtMinPages) {
    memory.grow(rtMinPages - havePages);
  }

  // Buffer-source instantiate returns `{ module, instance }`; a compiled
  // `WebAssembly.Module` returns the instance itself.
  const rtResult = await WebAssembly.instantiate(rtBytes, {
    env: {
      memory,
      abort_message(ptr, len) {
        const n = Number(len);
        const text =
          n > 0
            ? String.fromCharCode(...new Uint8Array(memory.buffer, Number(ptr), n))
            : "runtime abort";
        throw new WebAssembly.RuntimeError(text);
      },
      sysout_write: (...args) => env.sysout_write(...args),
    },
  });
  const rtInstance = rtResult.instance ?? rtResult;
  rtExports = rtInstance.exports;
  if (!rtExports) {
    throw new Error("simrt instantiate did not produce exports");
  }
  if (typeof env.attachRuntime === "function") {
    env.attachRuntime(rtExports);
  }
  return { module, instance, memory, rt: rtExports };
}

/**
 * @param {() => WebAssembly.Memory} getMemory
 * @param {(text: string) => void} writeStdout
 * @param {(text: string) => void} [writeStderr]
 */
export function createEnvImports(getMemory, writeStdout, writeStderr = null) {
  function mem() {
    const memory = getMemory();
    if (!memory) throw new WebAssembly.RuntimeError("wasm memory not ready");
    return memory;
  }

  function view() {
    return new DataView(mem().buffer);
  }

  function readFrame(frame) {
    const v = view();
    return {
      ptr: v.getUint32(frame, true),
      len: v.getInt32(frame + 4, true),
      pos: v.getInt32(frame + 8, true),
      pad: v.getInt32(frame + 12, true),
    };
  }

  function writePos(frame, pos) {
    view().setInt32(frame + 8, pos, true);
  }

  function contentBytes(frame) {
    const f = readFrame(frame);
    if (f.len <= 0) return null;
    return new Uint8Array(mem().buffer, f.ptr, f.len);
  }

  function contentString(frame) {
    const bytes = contentBytes(frame);
    if (!bytes) return null;
    return String.fromCharCode(...bytes);
  }

  function abortItem(message) {
    throw new WebAssembly.RuntimeError(message);
  }

  /** @type {WebAssembly.Exports | null} */
  let rtExports = null;
  function attachRuntime(exports) {
    rtExports = exports;
  }
  function requireRt() {
    if (!rtExports) {
      throw new WebAssembly.RuntimeError("sim runtime not attached");
    }
    return rtExports;
  }
  function rtFn(name) {
    const fn = requireRt()[`simrt_${name}`];
    if (typeof fn !== "function") {
      throw new WebAssembly.RuntimeError(`runtime missing simrt_${name}`);
    }
    return fn;
  }
  function latin1FromRt(dst, len) {
    const n = Number(len);
    if (n <= 0) return "";
    return String.fromCharCode(...new Uint8Array(mem().buffer, Number(dst), n));
  }
  function formatOutreal(value, digits, width, expDigits) {
    const dst = rtFn("format_scratch")();
    return latin1FromRt(
      dst,
      rtFn("format_out_real")(
        dst,
        rtFn("format_scratch_cap")(),
        value,
        BigInt(digits),
        BigInt(width),
        BigInt(expDigits ?? 2),
      ),
    );
  }
  function formatOutfix(value, digits, width) {
    const dst = rtFn("format_scratch")();
    return latin1FromRt(
      dst,
      rtFn("format_out_fix")(
        dst,
        rtFn("format_scratch_cap")(),
        value,
        BigInt(digits),
        BigInt(width),
      ),
    );
  }
  function formatOutfrac(value, digits, width) {
    const dst = rtFn("format_scratch")();
    return latin1FromRt(
      dst,
      rtFn("format_out_frac")(
        dst,
        rtFn("format_scratch_cap")(),
        BigInt(value),
        BigInt(digits),
        BigInt(width),
      ),
    );
  }

  // Match `parse_integer_item` in `src/runtime/text.rs`: optional sign, blanks
  // allowed after the sign, then a run of digits (no embedded blanks).
  function parseIntegerItem(input) {
    let index = 0;
    while (index < input.length && isBlank(input[index])) index += 1;
    let negative = false;
    let sawSign = false;
    if (index < input.length && (input[index] === "+" || input[index] === "-")) {
      negative = input[index] === "-";
      sawSign = true;
      index += 1;
    }
    while (index < input.length && isBlank(input[index])) index += 1;
    const numberStart = index;
    while (index < input.length && input[index] >= "0" && input[index] <= "9") {
      index += 1;
    }
    if (index === numberStart) return null;
    let value = Number(input.slice(numberStart, index));
    if (!Number.isSafeInteger(value)) abortItem("integer out of range");
    if (negative) value = -value;
    void sawSign;
    return { value, consumed: index };
  }

  function isBlank(ch) {
    return ch === " " || ch === "\t";
  }

  function isDecimalMark(ch) {
    return ch === "." || ch === ",";
  }

  // Match `parse_grouped_item_with` in `src/runtime/text.rs`: digit groups
  // separated by blanks, with an optional decimal-mark + fraction groups.
  // Trailing marks/blanks that are not followed by a digit are not consumed.
  function parseGroupedItem(input) {
    let index = 0;
    while (index < input.length && isBlank(input[index])) index += 1;
    let negative = false;
    if (index < input.length && (input[index] === "+" || input[index] === "-")) {
      negative = input[index] === "-";
      index += 1;
    }
    while (index < input.length && isBlank(input[index])) index += 1;

    function parseGroups() {
      if (index >= input.length || input[index] < "0" || input[index] > "9") {
        return false;
      }
      while (index < input.length && input[index] >= "0" && input[index] <= "9") {
        index += 1;
      }
      for (;;) {
        let look = index;
        while (look < input.length && isBlank(input[look])) look += 1;
        if (look > index && look < input.length && input[look] >= "0" && input[look] <= "9") {
          index = look;
          while (index < input.length && input[index] >= "0" && input[index] <= "9") {
            index += 1;
          }
        } else {
          break;
        }
      }
      return true;
    }

    const digitsStart = index;
    if (index < input.length && isDecimalMark(input[index])) {
      index += 1;
      if (!parseGroups()) return null;
    } else {
      if (!parseGroups()) return null;
      if (index < input.length && isDecimalMark(input[index])) {
        const beforeMark = index;
        const saved = index;
        index += 1;
        if (!parseGroups()) {
          index = beforeMark;
        }
        void saved;
      }
    }

    let digits = "";
    for (let i = digitsStart; i < index; i++) {
      const ch = input[i];
      if (ch >= "0" && ch <= "9") digits += ch;
    }
    if (!digits) return null;
    let value = Number(digits);
    if (!Number.isSafeInteger(value)) abortItem("grouped item out of range");
    if (negative) value = -value;
    return { value, consumed: index };
  }

  // Match `parse_real_item_with` in `src/runtime/text.rs`: blanks allowed after
  // the sign and inside the exponent, decimal mark `.`/`,`, lowten `&`/`e`/`E`.
  function parseRealItem(input) {
    let index = 0;
    const skipBlanks = () => {
      while (index < input.length && isBlank(input[index])) index += 1;
    };
    skipBlanks();
    let sign = "";
    if (index < input.length && (input[index] === "+" || input[index] === "-")) {
      sign = input[index];
      index += 1;
    }
    skipBlanks();
    const bodyStart = index;
    let sawDigit = false;
    while (index < input.length && input[index] >= "0" && input[index] <= "9") {
      sawDigit = true;
      index += 1;
    }
    if (index < input.length && isDecimalMark(input[index])) {
      index += 1;
      while (index < input.length && input[index] >= "0" && input[index] <= "9") {
        sawDigit = true;
        index += 1;
      }
    }
    if (
      index < input.length &&
      (input[index] === "&" || input[index] === "e" || input[index] === "E")
    ) {
      index += 1;
      skipBlanks();
      if (index < input.length && (input[index] === "+" || input[index] === "-")) {
        index += 1;
      }
      skipBlanks();
      const expStart = index;
      while (index < input.length && input[index] >= "0" && input[index] <= "9") {
        sawDigit = true;
        index += 1;
      }
      if (index === expStart) return null;
    }
    if (!sawDigit) return null;
    let token = sign + input.slice(bodyStart, index);
    token = token.replace(/,/g, ".").replace(/&/g, "e");
    token = token.split(/\s+/).join("");
    const value = Number(token);
    if (!Number.isFinite(value)) abortItem("real out of range");
    return { value, consumed: index };
  }

  function writeOut(text) {
    if (!writeStdout) {
      throw new WebAssembly.RuntimeError("stdout write not configured");
    }
    writeStdout(text);
  }

  function writeErr(text) {
    if (writeStderr) {
      writeStderr(text);
      return;
    }
    if (typeof process !== "undefined" && process.stderr) {
      process.stderr.write(text);
    }
  }

  function sysoutBase() {
    return view().getUint32(SYSOUT_BASE_PTR, true);
  }

  /**
   * Write `length` characters at the image's 1-based position, growing the
   * content and advancing the position — `simrt_image_out_text`.
   * `at(i)` yields the i-th character code of the source.
   */
  function imageOutChars(length, at) {
    if (length <= 0) return;
    const base = sysoutBase();
    const v = view();
    const buf = new Uint8Array(mem().buffer, base + IMAGE_OFF_BUF, IMAGE_BUF_SIZE);
    let len = v.getInt32(base + IMAGE_OFF_LEN, true);
    let pos = v.getInt32(base + IMAGE_OFF_POS, true);
    let start = pos > 0 ? pos - 1 : 0;
    if (start > len) {
      let pad = start - len;
      if (len + pad > IMAGE_BUF_SIZE) {
        pad = IMAGE_BUF_SIZE > len ? IMAGE_BUF_SIZE - len : 0;
      }
      buf.fill(32, len, len + pad);
      len += pad;
      start = len;
    }
    for (let i = 0; i < length; i++) {
      const to = start + i;
      if (to >= IMAGE_BUF_SIZE) break;
      buf[to] = at(i);
      if (to >= len) len = to + 1;
    }
    pos = start + length + 1;
    if (pos > IMAGE_BUF_SIZE + 1) pos = IMAGE_BUF_SIZE + 1;
    v.setInt32(base + IMAGE_OFF_LEN, len, true);
    v.setInt32(base + IMAGE_OFF_MAIN_LEN, len, true);
    v.setInt32(base + IMAGE_OFF_POS, pos, true);
  }

  function sysoutWrite(ptr, length) {
    const src = new Uint8Array(mem().buffer);
    imageOutChars(Number(length), (i) => src[Number(ptr) + i]);
  }

  function fdWrite(_fd, iovs, iovsLen, nwritten) {
    const memory = mem();
    const v = new DataView(memory.buffer);
    let written = 0;
    for (let i = 0; i < Number(iovsLen); i++) {
      const base = Number(iovs) + i * 8;
      const ptr = v.getUint32(base, true);
      const len = v.getUint32(base + 4, true);
      if (len > 0) {
        writeOut(
          new TextDecoder("latin1").decode(new Uint8Array(memory.buffer, ptr, len)),
        );
        written += len;
      }
    }
    v.setUint32(Number(nwritten), written, true);
    return 0;
  }

  function fdRead(_fd, _iovs, _iovsLen, nread) {
    new DataView(mem().buffer).setUint32(Number(nread), 0, true);
    return 0;
  }

  function sysoutWriteString(text) {
    imageOutChars(text.length, (i) => text.charCodeAt(i) & 0xff);
  }

  /**
   * `outimage` writes the whole image; `breakoutimage` only the part before the
   * current position. Both terminate the record and reset the image to blanks
   * with the position back at 1 (§10.5.6 / §10.7.3).
   */
  function sysoutFlush(breakOnly) {
    const base = sysoutBase();
    const v = view();
    const len = v.getInt32(base + IMAGE_OFF_LEN, true);
    const pos = v.getInt32(base + IMAGE_OFF_POS, true);
    let count = len;
    if (breakOnly) {
      count = Math.min(Math.max(pos - 1, 0), len);
    }
    const buf = new Uint8Array(mem().buffer, base + IMAGE_OFF_BUF, IMAGE_BUF_SIZE);
    // The image is a fixed-width blank record, so its unused tail is padding
    // rather than output.
    while (count > 0 && buf[count - 1] === 32) count -= 1;
    writeOut(String.fromCharCode(...buf.subarray(0, count)) + "\n");
    buf.fill(32, 0, len);
    v.setInt32(base + IMAGE_OFF_POS, 1, true);
    v.setInt32(base + IMAGE_OFF_LINE, v.getInt32(base + IMAGE_OFF_LINE, true) + 1, true);
  }

  function fieldViaEditNumeric(fieldWidth, item) {
    const width = Number(fieldWidth);
    if (width < 0) abortItem("field width < 0");
    if (item.length > width) return "*".repeat(width);
    return item.padStart(width, " ");
  }

  /** @type {Map<number, object>} */
  const diskFiles = new Map();

  function bumpAlloc(size) {
    const v = view();
    const cursor = v.getUint32(HEAP_CURSOR, true);
    v.setUint32(HEAP_CURSOR, cursor + size, true);
    return cursor;
  }

  function writeTextFrame(destFrame, ptr, len, pos = 1, pad = 0, start = 1, mainLen = len) {
    const v = view();
    v.setUint32(destFrame + FRAME_OFF_PTR, ptr, true);
    v.setInt32(destFrame + FRAME_OFF_LEN, len, true);
    v.setInt32(destFrame + FRAME_OFF_POS, pos, true);
    v.setInt32(destFrame + FRAME_OFF_PAD, pad, true);
    v.setInt32(destFrame + FRAME_OFF_START, start, true);
    v.setInt32(destFrame + FRAME_OFF_MAIN_LEN, mainLen, true);
  }

  function framePtr(frame) {
    return Number(frame) >>> 0;
  }

  function requireHandle(obj) {
    const key = Number(obj) >>> 0;
    const handle = diskFiles.get(key);
    if (!handle) {
      abortItem("basicio: unknown file object");
    }
    return handle;
  }

  function blankImage(len) {
    return " ".repeat(len);
  }

  function diskOutChars(handle, length, at) {
    if (length <= 0) return;
    let image = handle.image;
    let pos = handle.pos;
    let start = pos > 0 ? pos - 1 : 0;
    if (start > handle.imageLen) {
      image += " ".repeat(start - handle.imageLen);
      handle.imageLen = start;
    }
    const chars = image.split("");
    for (let i = 0; i < length; i++) {
      const to = start + i;
      while (chars.length <= to) chars.push(" ");
      chars[to] = String.fromCharCode(at(i));
      if (to >= handle.imageLen) handle.imageLen = to + 1;
    }
    handle.image = chars.join("");
    handle.pos = start + length + 1;
  }

  function diskOutText(handle, ptr, len) {
    const src = new Uint8Array(mem().buffer);
    diskOutChars(handle, Number(len), (i) => src[Number(ptr) + i]);
  }

  function diskOutChar(handle, ch) {
    const cp = Number(ch) & 0xffffffff;
    if (cp <= 0x7f) {
      diskOutChars(handle, 1, () => cp);
      return;
    }
    let buf;
    if (cp <= 0x7ff) {
      buf = [0xc0 | (cp >> 6), 0x80 | (cp & 0x3f)];
    } else if (cp <= 0xffff) {
      buf = [0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f)];
    } else {
      buf = [
        0xf0 | (cp >> 18),
        0x80 | ((cp >> 12) & 0x3f),
        0x80 | ((cp >> 6) & 0x3f),
        0x80 | (cp & 0x3f),
      ];
    }
    diskOutChars(handle, buf.length, (i) => buf[i]);
  }

  function trimTrailingSpaces(text, count) {
    let n = count;
    while (n > 0 && text.charCodeAt(n - 1) === 32) n -= 1;
    return text.slice(0, n);
  }

  function writeLineToFile(handle, payload, withNewline = true) {
    if (!handle.fd) abortItem("OutFile: file is not open");
    fs().writeSync(handle.fd, payload);
    if (withNewline) fs().writeSync(handle.fd, "\n");
    fs().fsyncSync(handle.fd);
  }

  function normalizeFilename(path) {
    if (path === null) return null;
    const line = path.split(/[\r\n]/)[0];
    return line.trim();
  }

  function basicioRegister(obj, pathFrame, mode) {
    const modeNum = Number(mode);
    const key = Number(obj) >>> 0;
    if (diskFiles.has(key)) return;
    // §10.1 rejects a notext FILENAME, and nothing more: a name that is only
    // blanks is a legal (if unopenable) one, and `filename` has to give it back
    // exactly as it was passed. Only the filesystem sees the trimmed form.
    const name = contentString(framePtr(pathFrame));
    if (name === null || name.length === 0) {
      abortItem("file: FILENAME is notext");
    }
    const path = normalizeFilename(name);
    // Modes: 0=In, 1=Out, 2=InByte, 3=OutByte, 4=Direct, 5=DirectByte, 6=Print.
    diskFiles.set(key, {
      mode: modeNum,
      name,
      path,
      open: false,
      endfile: false,
      line: 1,
      page: 0,
      spacing: 1,
      linesPerPage: 60,
      defaultLinesPerPage: 60,
      append: false,
      createMode: "anycreate",
      image: "",
      imageLen: 0,
      pos: 1,
      fd: null,
      loc: 1,
      maxloc: Number.MAX_SAFE_INTEGER - 1,
      /** @type {Map<number, string>} */
      directImages: new Map(),
      /** @type {Buffer} */
      byteStore: Buffer.alloc(0),
    });
  }

  function directLastloc(handle) {
    let last = 0;
    for (const loc of handle.directImages.keys()) {
      if (loc > last) last = loc;
    }
    return last;
  }

  function locateDirect(handle, loc) {
    const i = Number(loc);
    if (i < 1 || i > handle.maxloc) {
      abortItem("locate: parameter out of range");
    }
    handle.loc = i;
  }

  function basicioOpen(obj, imageFrame) {
    const handle = requireHandle(obj);
    if (handle.open) return 0n;
    if (handle.mode === 4) {
      const frame = framePtr(imageFrame);
      const f = readFrame(frame);
      if (f.len <= 0) abortItem("open: fileimage is notext");
      const imageLen = f.len;
      handle.imageLen = imageLen;
      handle.image = blankImage(imageLen);
      handle.pos = 1;
      handle.loc = 1;
      handle.maxloc = Number.MAX_SAFE_INTEGER - 1;
      handle.directImages = new Map();
      const exists = fs().existsSync(handle.path);
      if (exists) {
        try {
          let contents = fs().readFileSync(handle.path, "utf8");
          while (contents.endsWith("\n") || contents.endsWith("\r")) {
            contents = contents.slice(0, -1);
          }
          const lines = contents.length === 0 ? [] : contents.split("\n");
          for (let i = 0; i < lines.length; i++) {
            handle.directImages.set(i + 1, lines[i]);
          }
        } catch {
          return 0n;
        }
      }
      handle.open = true;
      handle.endfile = false;
      return 1n;
    }
    if (handle.mode === 2 || handle.mode === 3 || handle.mode === 5) {
      throw new WebAssembly.RuntimeError("bytefile open requires basicio_open_byte");
    }
    const frame = framePtr(imageFrame);
    const f = readFrame(frame);
    if (f.len <= 0) abortItem("open: fileimage is notext");
    const imageLen = f.len;
    if (handle.mode === 0) {
      try {
        handle.fd = fs().openSync(handle.path, "r");
      } catch {
        return 0n;
      }
      handle.image = blankImage(imageLen);
      handle.imageLen = imageLen;
      handle.pos = imageLen + 1;
    } else {
      const exists = fs().existsSync(handle.path);
      if (handle.createMode === "create" && exists) return 0n;
      if (handle.createMode === "nocreate" && !exists) return 0n;
      try {
        handle.fd = fs().openSync(
          handle.path,
          handle.append ? "a" : "w",
        );
      } catch {
        return 0n;
      }
      handle.image = blankImage(imageLen);
      handle.imageLen = imageLen;
      handle.pos = 1;
      handle.line = 1;
      handle.spacing = 1;
      if (!handle.linesPerPage) handle.linesPerPage = 60;
    }
    handle.open = true;
    handle.endfile = false;
    return 1n;
  }

  function basicioOpenByte(obj) {
    const handle = requireHandle(obj);
    if (handle.open) return 0n;
    const exists = fs().existsSync(handle.path);
    if (handle.mode === 2) {
      if (!exists) return 0n;
      try {
        handle.fd = fs().openSync(handle.path, "r");
      } catch {
        return 0n;
      }
      handle.open = true;
      handle.endfile = false;
      return 1n;
    }
    if (handle.mode === 3) {
      try {
        handle.fd = fs().openSync(handle.path, "w");
      } catch {
        return 0n;
      }
      handle.open = true;
      handle.endfile = false;
      return 1n;
    }
    if (handle.mode === 5) {
      handle.byteStore = Buffer.alloc(0);
      if (exists) {
        try {
          handle.byteStore = fs().readFileSync(handle.path);
        } catch {
          return 0n;
        }
      }
      handle.loc = 1;
      handle.maxloc = Number.MAX_SAFE_INTEGER - 1;
      handle.open = true;
      handle.endfile = false;
      return 1n;
    }
    abortItem("open: bytefile requires parameterless open");
  }

  function basicioClose(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) return 0n;
    if (handle.mode === 4) {
      const last = directLastloc(handle);
      const lines = [];
      for (let loc = 1; loc <= last; loc++) {
        lines.push(handle.directImages.get(loc) ?? "");
      }
      const body = lines.join("\n");
      fs().writeFileSync(handle.path, body.length > 0 ? `${body}\n` : "");
      handle.directImages.clear();
      handle.loc = 0;
      handle.maxloc = 0;
    } else if (handle.mode === 5) {
      fs().writeFileSync(handle.path, handle.byteStore);
      handle.byteStore = Buffer.alloc(0);
      handle.maxloc = 0;
    } else if (handle.mode !== 0 && handle.pos !== 1 && handle.fd !== null) {
      writeLineToFile(handle, handle.image.slice(0, handle.imageLen));
      handle.image = blankImage(handle.imageLen);
      handle.pos = 1;
    }
    if (handle.fd !== null) {
      fs().closeSync(handle.fd);
      handle.fd = null;
    }
    handle.open = false;
    handle.endfile = true;
    handle.imageLen = 0;
    handle.image = "";
    handle.pos = 1;
    return 1n;
  }

  function basicioIsopen(obj) {
    return requireHandle(obj).open ? 1n : 0n;
  }

  function basicioOutText(obj, ptr, len) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("OutFile.outtext: file is not open");
    const tLen = Number(len);
    if (handle.pos > 1 && tLen > handle.imageLen - handle.pos + 1) {
      basicioOutImage(obj);
    }
    diskOutText(handle, ptr, len);
  }

  function basicioOutChar(obj, ch) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("OutFile.outchar: file is not open");
    if (handle.pos > handle.imageLen) {
      basicioOutImage(obj);
    }
    diskOutChar(handle, ch);
  }

  function basicioOutImage(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("OutFile.outimage: file is not open");
    if (handle.mode === 0) {
      handle.image = blankImage(handle.imageLen);
      handle.pos = 1;
      return;
    }
    if (handle.mode === 4) {
      if (handle.loc > handle.maxloc) abortItem("outimage: file overflow");
      handle.directImages.set(handle.loc, handle.image.slice(0, handle.imageLen));
      locateDirect(handle, handle.loc + 1);
      handle.image = blankImage(handle.imageLen);
      handle.pos = 1;
      return;
    }
    if (handle.fd === null) abortItem("OutFile.outimage: file is not open");
    writeLineToFile(handle, handle.image.slice(0, handle.imageLen));
    handle.image = blankImage(handle.imageLen);
    handle.pos = 1;
    if (handle.mode === 6) {
      handle.line += handle.spacing > 0 ? handle.spacing : 1;
    } else {
      handle.line += 1;
    }
  }

  function basicioBreakOutImage(obj) {
    const handle = requireHandle(obj);
    if (!handle.open || handle.fd === null) {
      abortItem("OutFile.breakoutimage: file is not open");
    }
    const breakLen = Math.min(Math.max(handle.pos - 1, 0), handle.imageLen);
    writeLineToFile(handle, handle.image.slice(0, breakLen));
    handle.image = blankImage(handle.imageLen);
    handle.pos = 1;
    handle.line += 1;
  }

  function readLineFromFd(fd) {
    const chunks = [];
    const buf = Buffer.alloc(1);
    while (true) {
      const n = fs().readSync(fd, buf, 0, 1, null);
      if (n <= 0) {
        return chunks.length === 0 ? null : Buffer.from(chunks).toString("utf8");
      }
      const ch = buf[0];
      if (ch === 0x0a) {
        return Buffer.from(chunks).toString("utf8");
      }
      if (ch !== 0x0d) {
        chunks.push(ch);
      }
    }
  }

  function basicioInImage(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("InFile.inimage: file is not open");
    if (handle.mode === 4) {
      handle.pos = 1;
      const last = directLastloc(handle);
      handle.endfile = handle.loc > last;
      if (handle.endfile) {
        handle.image = blankImage(handle.imageLen);
        if (handle.imageLen > 0) {
          handle.image = EM_CHAR + handle.image.slice(1);
        } else {
          handle.image = EM_CHAR;
        }
        handle.pos = 1;
      } else if (handle.directImages.has(handle.loc)) {
        const line = handle.directImages.get(handle.loc);
        handle.image = blankImage(handle.imageLen);
        if (line.length > 0) {
          handle.image = line + handle.image.slice(line.length);
        }
        handle.pos = 1;
      } else {
        handle.image = "\0".repeat(handle.imageLen);
        handle.pos = handle.imageLen + 1;
      }
      locateDirect(handle, handle.loc + 1);
      return;
    }
    if (handle.endfile) abortItem("inimage: end of file");
    if (handle.fd === null) abortItem("InFile.inimage: file is not open");
    const line = readLineFromFd(handle.fd);
    if (line === null) {
      handle.endfile = true;
      handle.image = blankImage(handle.imageLen);
      if (handle.imageLen > 0) {
        handle.image = EM_CHAR + handle.image.slice(1);
      }
      handle.pos = 1;
      return;
    }
    if (line.length > handle.imageLen) {
      abortItem("inimage: image too short for external image");
    }
    handle.image = blankImage(handle.imageLen);
    if (line.length > 0) {
      handle.image = line + handle.image.slice(line.length);
    }
    handle.pos = 1;
    handle.endfile = false;
  }

  function basicioInChar(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("InFile.inchar: file is not open");
    if (handle.mode === 4) {
      while (handle.pos > handle.imageLen) {
        basicioInImage(obj);
      }
      if (handle.endfile && handle.pos > handle.imageLen) {
        abortItem("InChar: end of file");
      }
    } else if (handle.pos > handle.imageLen) {
      basicioInImage(obj);
    }
    if (handle.endfile && handle.pos > handle.imageLen) {
      abortItem("InChar: end of file");
    }
    const idx = handle.pos > 0 ? handle.pos - 1 : 0;
    if (idx >= handle.imageLen) {
      abortItem("InChar: no more characters in image");
    }
    const ch = handle.image.charCodeAt(idx);
    handle.pos += 1;
    return BigInt(ch);
  }

  function basicioEndfile(obj) {
    return requireHandle(obj).endfile ? 1n : 0n;
  }

  function basicioImage(obj, destFrame) {
    const handle = requireHandle(obj);
    const dest = framePtr(destFrame);
    if (!handle.open || handle.imageLen <= 0) {
      writeTextFrame(dest, 0, 0, 1, 1, 1, 0);
      return;
    }
    const len = handle.imageLen;
    const ptr = bumpAlloc(len);
    const bytes = new Uint8Array(mem().buffer, ptr, len);
    for (let i = 0; i < len; i++) {
      bytes[i] = handle.image.charCodeAt(i) & 0xff;
    }
    writeTextFrame(dest, ptr, len, 1, 0, 1, len);
  }

  function basicioSetImage(obj, srcFrame) {
    const handle = requireHandle(obj);
    const src = readFrame(framePtr(srcFrame));
    const copyLen = Math.min(Math.max(src.len, 0), handle.imageLen);
    let image = blankImage(handle.imageLen);
    if (copyLen > 0) {
      const bytes = new Uint8Array(mem().buffer, src.ptr, copyLen);
      image = String.fromCharCode(...bytes) + image.slice(copyLen);
    }
    handle.image = image;
    handle.pos = 1;
  }

  function basicioPos(obj) {
    const handle = requireHandle(obj);
    return BigInt(handle.pos > 0 ? handle.pos : 1);
  }

  function basicioLength(obj) {
    return BigInt(requireHandle(obj).imageLen);
  }

  function basicioSetpos(obj, index) {
    const handle = requireHandle(obj);
    const i = Number(index);
    if (i <= 0) {
      handle.pos = 1;
    } else if (i > handle.imageLen + 1) {
      handle.pos = handle.imageLen + 1;
    } else {
      handle.pos = i;
    }
  }

  function basicioLine(obj) {
    return BigInt(requireHandle(obj).line);
  }

  function basicioSetaccess(obj, modeFrame) {
    const handle = requireHandle(obj);
    const raw = contentString(framePtr(modeFrame));
    if (raw === null) return 0n;
    const mode = raw.trim().toLowerCase();
    switch (mode) {
      case "append":
        handle.append = true;
        return 1n;
      case "noappend":
        handle.append = false;
        return 1n;
      case "create":
        handle.createMode = "create";
        return 1n;
      case "nocreate":
        handle.createMode = "nocreate";
        return 1n;
      case "anycreate":
        handle.createMode = "anycreate";
        return 1n;
      case "shared":
      case "noshared":
      case "readonly":
      case "writeonly":
      case "readwrite":
      case "rewind":
      case "norewind":
      case "purge":
      case "nopurge":
        return 1n;
      default:
        if (mode.startsWith("bytesize:")) return 1n;
        return 0n;
    }
  }

  function basicioEject(obj, nIn) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("eject: file is not open");
    let n = Number(nIn);
    if (!(n > 0)) abortItem("eject: parameter out of range");
    const lpp = handle.linesPerPage > 0 ? handle.linesPerPage : 60;
    if (n > lpp) n = 1;
    if (n <= handle.line) {
      if (handle.fd !== null) {
        writeLineToFile(handle, "");
      }
      handle.page = (handle.page ?? 0) + 1;
    }
    handle.line = n;
  }

  function basicioLinesperpage(obj, nIn) {
    const handle = requireHandle(obj);
    const prev = handle.linesPerPage > 0 ? handle.linesPerPage : 60;
    const n = Number(nIn);
    if (!handle.defaultLinesPerPage) handle.defaultLinesPerPage = 60;
    if (n > 0) handle.linesPerPage = n;
    else if (n < 0) handle.linesPerPage = Number.MAX_SAFE_INTEGER;
    else handle.linesPerPage = handle.defaultLinesPerPage;
    return BigInt(prev);
  }

  function basicioInrecord(obj) {
    const handle = requireHandle(obj);
    if (!handle.open || handle.endfile || handle.fd === null) {
      abortItem("inrecord: file closed or at endfile");
    }
    const line = readLineFromFd(handle.fd);
    if (line === null) {
      handle.endfile = true;
      handle.pos = 1;
      return 0n;
    }
    let text = line;
    while (text.endsWith("\n") || text.endsWith("\r")) {
      text = text.slice(0, -1);
    }
    const capacity = handle.imageLen;
    const truncated = text.length > capacity;
    const take = Math.min(text.length, capacity);
    handle.image = blankImage(capacity);
    for (let i = 0; i < take; i++) {
      handle.image =
        handle.image.slice(0, i) + text[i] + handle.image.slice(i + 1);
    }
    handle.pos = take + 1;
    return truncated ? 1n : 0n;
  }

  function basicioFilename(obj, destFrame) {
    const handle = requireHandle(obj);
    const dest = framePtr(destFrame);
    const name = handle.name ?? handle.path ?? "";
    const len = name.length;
    const ptr = bumpAlloc(len);
    const bytes = new Uint8Array(mem().buffer, ptr, len);
    for (let i = 0; i < len; i++) {
      bytes[i] = name.charCodeAt(i) & 0xff;
    }
    writeTextFrame(dest, ptr, len, 1, 0, 1, len);
  }

  function basicioSkipSpaces(obj, handle) {
    let ch = 32;
    while (!handle.endfile && (ch === 32 || ch === 9)) {
      if (handle.pos > handle.imageLen) {
        basicioInImage(obj);
        if (handle.endfile && handle.pos > handle.imageLen) break;
      }
      ch = Number(basicioInChar(obj));
    }
    return ch;
  }

  function basicioLastitem(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("lastitem: file is not open");
    const ch = basicioSkipSpaces(obj, handle);
    if (!handle.endfile && ch !== 32 && ch !== 9 && handle.pos > 1) {
      handle.pos -= 1;
    }
    return handle.endfile ? 1n : 0n;
  }

  function remainingImageString(handle) {
    const pos = handle.pos > 0 ? handle.pos : 1;
    if (pos > handle.imageLen) return "";
    return handle.image.slice(pos - 1, handle.imageLen);
  }

  function basicioInint(obj) {
    if (basicioLastitem(obj)) abortItem("inint: end of file");
    const handle = requireHandle(obj);
    const parsed = parseIntegerItem(remainingImageString(handle));
    if (!parsed) abortItem("inint: no numeric item");
    handle.pos += parsed.consumed;
    return BigInt(parsed.value);
  }

  function basicioInreal(obj) {
    if (basicioLastitem(obj)) abortItem("inreal: end of file");
    const handle = requireHandle(obj);
    const parsed = parseRealItem(remainingImageString(handle));
    if (!parsed) abortItem("inreal: no numeric item");
    handle.pos += parsed.consumed;
    return parsed.value;
  }

  function basicioInfrac(obj) {
    if (basicioLastitem(obj)) abortItem("infrac: end of file");
    const handle = requireHandle(obj);
    const parsed = parseGroupedItem(remainingImageString(handle));
    if (!parsed) abortItem("infrac: no numeric item");
    handle.pos += parsed.consumed;
    return BigInt(parsed.value);
  }

  function basicioIntext(obj, width) {
    const w = Number(width);
    if (w <= 0) {
      const frame = bumpAlloc(FRAME_SIZE);
      writeTextFrame(frame, 0, -1, 1, 0, 1, 0);
      return BigInt(frame);
    }
    const ptr = bumpAlloc(w);
    const bytes = new Uint8Array(mem().buffer, ptr, w);
    for (let i = 0; i < w; i++) {
      bytes[i] = Number(basicioInChar(obj)) & 0xff;
    }
    const frame = bumpAlloc(FRAME_SIZE);
    writeTextFrame(frame, ptr, w, 1, 0, 1, w);
    return BigInt(frame);
  }

  function basicioOutReal(obj, value, digits, width, expDigits) {
    basicioOutTextFromString(obj, formatOutreal(value, digits, width, expDigits));
  }

  function basicioOutFix(obj, value, digits, width) {
    basicioOutTextFromString(obj, formatOutfix(value, digits, width));
  }

  function basicioOutFrac(obj, value, digits, width) {
    basicioOutTextFromString(obj, formatOutfrac(value, digits, width));
  }

  function basicioOutTextFromString(obj, text) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("OutFile.outtext: file is not open");
    const tLen = text.length;
    if (handle.pos > 1 && tLen > handle.imageLen - handle.pos + 1) {
      basicioOutImage(obj);
    }
    diskOutChars(handle, tLen, (i) => text.charCodeAt(i) & 0xff);
  }

  function basicioOutInt(obj, value, width) {
    const w = Number(width);
    const item = String(Number(value));
    let field;
    if (w === 0) {
      field = item;
    } else {
      field = fieldViaEditNumeric(Math.abs(w), item);
    }
    basicioOutTextFromString(obj, field);
  }

  function basicioInByte(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("inbyte: file is not open");
    if (handle.mode === 2) {
      if (handle.endfile) abortItem("inbyte: end of file");
      if (handle.fd === null) abortItem("inbyte: no reader");
      const buf = Buffer.alloc(1);
      const n = fs().readSync(handle.fd, buf, 0, 1, null);
      if (n <= 0) {
        handle.endfile = true;
        return 0n;
      }
      return BigInt(buf[0]);
    }
    if (handle.mode === 5) {
      const last = handle.byteStore.length;
      if (handle.loc <= last) {
        const b = handle.byteStore[handle.loc - 1];
        handle.loc += 1;
        return BigInt(b);
      }
      return 0n;
    }
    abortItem("inbyte: not an inbytefile");
  }

  function basicioOutByte(obj, byte) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("outbyte: file is not open");
    const x = Number(byte);
    if (x < 0 || x > 255) abortItem("outbyte: illegal byte value");
    if (handle.mode === 3) {
      if (handle.fd === null) abortItem("outbyte: no writer");
      fs().writeSync(handle.fd, Buffer.from([x]));
      return;
    }
    if (handle.mode === 5) {
      if (handle.loc > handle.maxloc) abortItem("outbyte: file overflow");
      const idx = handle.loc - 1;
      if (idx >= handle.byteStore.length) {
        const grown = Buffer.alloc(idx + 1);
        handle.byteStore.copy(grown);
        handle.byteStore = grown;
      }
      handle.byteStore[idx] = x;
      handle.loc += 1;
      return;
    }
    abortItem("outbyte: not an outbytefile");
  }

  function basicioLocate(obj, loc) {
    const handle = requireHandle(obj);
    if (handle.mode !== 4 && handle.mode !== 5) {
      abortItem("locate: not a directfile");
    }
    if (!handle.open) abortItem("locate: file is not open");
    locateDirect(handle, loc);
  }

  function basicioLocation(obj) {
    return BigInt(requireHandle(obj).loc);
  }

  function basicioLastloc(obj) {
    const handle = requireHandle(obj);
    if (!handle.open) abortItem("lastloc: file closed");
    if (handle.mode === 4) {
      return BigInt(directLastloc(handle));
    }
    if (handle.mode === 5) {
      return BigInt(handle.byteStore.length);
    }
    abortItem("lastloc: not a direct file");
  }

  function envError(textFrame) {
    const text = contentString(framePtr(textFrame));
    writeErr(`sim: ${text ?? ""}\n`);
    throw new WebAssembly.RuntimeError(text ?? "error");
  }

  function diskBasicioStub(name) {
    return () => {
      throw new WebAssembly.RuntimeError(`${name}: disk BASICIO not available in browser`);
    };
  }

  return {
    attachRuntime,
    fd_write: fdWrite,
    fd_read: fdRead,
    sysout_write: sysoutWrite,
    sysout_flush: sysoutFlush,
    basicio_register: basicioRegister,
    basicio_open: basicioOpen,
    basicio_close: basicioClose,
    basicio_isopen: basicioIsopen,
    basicio_out_text: basicioOutText,
    basicio_out_char: basicioOutChar,
    basicio_out_image: basicioOutImage,
    basicio_break_out_image: basicioBreakOutImage,
    basicio_in_image: basicioInImage,
    basicio_in_char: basicioInChar,
    basicio_endfile: basicioEndfile,
    basicio_image: basicioImage,
    basicio_set_image: basicioSetImage,
    basicio_pos: basicioPos,
    basicio_length: basicioLength,
    basicio_setpos: basicioSetpos,
    basicio_line: basicioLine,
    basicio_filename: basicioFilename,
    basicio_lastitem: basicioLastitem,
    basicio_inint: basicioInint,
    basicio_inreal: basicioInreal,
    basicio_infrac: basicioInfrac,
    basicio_intext: basicioIntext,
    basicio_out_real: basicioOutReal,
    basicio_out_fix: basicioOutFix,
    basicio_out_frac: basicioOutFrac,
    basicio_out_int: basicioOutInt,
    error: envError,
    basicio_open_byte: basicioOpenByte,
    basicio_in_byte: basicioInByte,
    basicio_out_byte: basicioOutByte,
    basicio_locate: basicioLocate,
    basicio_location: basicioLocation,
    basicio_lastloc: basicioLastloc,
    basicio_setaccess: basicioSetaccess,
    basicio_eject: basicioEject,
    basicio_linesperpage: basicioLinesperpage,
    basicio_inrecord: basicioInrecord,
    diskBasicioStub,
  };
}
