# Interop tutorial

How a Simula program compiled or interpreted by **sim** talks to C, JavaScript,
Rust, Python, and other Simula modules.

This page is the hands-on path: each section matches a directory under
[`examples/`](../examples/). Build the compiler once (`cargo build`), then run
any example with `./run.sh`.

## Two products

Interop is two directions that share one type algebra.

| Product | Who owns `main` | Simula source | Typical host |
| --- | --- | --- | --- |
| **Foreign procedures** | Simula | `external C/JS/Host procedure … is …` | A `.o`, `libm`, `console.log`, a Rust closure |
| **Embedding** | The host | Native `--crate-type lib`; wasm public exports | C `sim_*` / `simrt_call`, JS `exports.tick`, Python `ctypes`, Rust `Interpreter::call` |

A **program** (`sim compile` default) still has `_start` / `sim_main` and
may *also* export. A native **library** (`--crate-type lib`) has no process
entry; the host loads it and calls in. On wasm `--crate-type` is ignored:
every module exports `_start` plus public procedures, and the host calls
whichever it needs.

Stdlib lines such as `procedure sin(r); external;` are compiler builtins
(§6.3.7), not user FFI. Write `external C procedure csqrt = "libm:sqrt"` when
you really want libc.

## What can cross the boundary

| Simula | C | Wasm | Interpreter |
| --- | --- | --- | --- |
| `integer` | `int64_t` | `i64` | `Value::I64` |
| `real` | `double` | `f64` | `Value::F64` |
| `boolean` | `int32_t` 0/1 | `i32` | `Value::Bool` |
| `character` | `uint32_t` rank | `i32` | rank as `I64` |
| `text` (by `value`) | `const uint8_t *` + length | JS/Host: JS `String`; C: `(ptr, len)` scratch | cloned `Value::Text` |
| `ref (C)` | opaque `void *` (pin it) | WasmGC `eqref` | `Value::ObjectRef` (root it) |

Not foreign: name parameters, arrays, formal procedures, labels, host-defined
classes. `text` is always a **copy**; do not keep the C pointer after return.

`--charset latin1` (default) is one byte per rank 0–255. `--charset utf8`
encodes those ranks as UTF-8 at the C edge, and as a JS `String` at the
JS/Host edge. Interpreter `Host` clones frames and never encodes.
Instantiate wasm with `instantiateSimulaWasm` from the `wasm_host.mjs`
written next to the module (`--no-wasm-host` skips it). Linear memory and
text copies stay in that helper. Application JS passes `console` / `host` /
`js` and calls exports; it does not read `memory.buffer`.

## Kinds and identification

```simula
external C procedure add = "add"          ! native symbol, or "libm:sqrt"
   is integer procedure add(a, b); integer a, b;

external JS procedure log = "console.log" ! import object path
   is procedure log(msg); value msg; text msg;

external Host procedure draw = "draw"     ! portable: interp + native table + wasm "host"
   is procedure draw(x, y); real x, y;

external integer procedure helper = "utils";  ! other Simula, file stem utils.sim
```

`Host` is the spelling to use when the same source should run under
`sim run` (Rust), native (`simrt_instantiate`), and wasm
(`import "host"`). `C` and `JS` name a concrete ABI.

Library exports: a top-level procedure becomes `sim_<name>` on native and the
raw name on wasm. `= "export:plus"` picks an exact symbol.

## Tutorial path

Work through the examples in order. Each README repeats the exact commands.

1. **[Simula calls C](../examples/01-c-from-simula/)** — link `add.o`, print `42`.
2. **[C calls Simula](../examples/02-simula-from-c/)** — `--crate-type lib`, `sim_add`.
3. **[C host table](../examples/03-c-host-embed/)** — `simrt_instantiate` supplies
   `Host` imports and `simrt_call`s an export.
4. **[`libm:sqrt`](../examples/04-libm/)** — identification `lib:symbol` (no
   `dlopen`; the host linker sees `-lm` where needed).
5. **[Simula calls JS](../examples/05-js-from-simula/)** — wasm `console.log`
   receives a JS `string`.
6. **[JS calls Simula](../examples/06-js-embeds-simula/)** — wasm exports,
   `exports.add(40n, 2n)` (host does not run `_start`).
7. **[Rust interpreter host](../examples/07-rust-host/)** — `define_host` +
   `Interpreter::call`. Same Simula as a canvas host would use (`plot`, not
   ENVIRONMENT `draw`).
8. **[Simula `--with`](../examples/08-simula-with/)** — Chapter 6: `utils.sim`
   provides `helper`, not source concatenation.
9. **[UTF-8 text](../examples/09-utf8-text/)** — `--charset utf8`, rank 233 →
   `C3 A9`.
10. **[Opaque `ref`](../examples/10-ref-handles/)** — C pins a Simula object
    across `gc_collect` and returns it.
11. **[Python `ctypes`](../examples/11-python-ctypes/)** — the native C ABI is
    the stable surface for “any language that can call C”.
12. **[Browser model](../examples/12-browser-canvas/)** — JS animation loop
    calls exported `tick`; Simula calls `Host plot`.

## Embedding cheat sheet

**C** (link the `--crate-type lib` artifact, include `runtime/embed.h`):

```c
SimrtHostDef host[] = {{"add", (void *)host_add}};
SimrtInstance *s = simrt_instantiate(host, 1);
SimrtVal result;
simrt_call(s, "sim_combo", NULL, 0, &result);
simrt_release(s);
```

Or call `sim_add` directly if you do not need a Host table.

**JavaScript** (wasm; instantiate with the generated `wasm_host.mjs`):

```js
const { instance } = await instantiateSimulaWasm(module, {
  host: { plot(x, y) { /* … */ } },
  console: { log(msg) { /* msg is a JS string */ } },
});
instance.exports.tick(dt);
```

**Rust** (crate `outimage`, interpreter backend):

```rust
let mut vm = Interpreter::from_module(&module);
vm.define_host("plot", |_ctx, args| {
    let _ = (args[0].as_f64()?, args[1].as_f64()?);
    Ok(Value::None)
});
let five = vm.call("hypot", &[Value::F64(3.0), Value::F64(4.0)])?;
```

Host closures that keep a `text` or `ref` must `ctx.root(value)` (interpreter)
or `simrt_ref_pin` (native). Dropping the handle unroots. There is no
observable GC at the boundary.

## Simulation

A closed program still runs until the SQS is empty. An embedded host should
call `simrt_sim_step` / wasm `step` between UI frames. A foreign call is
**instantaneous**: it must not `hold` / `passivate`. Asynchronous work on
native is a Host callback that returns immediately and later `simrt_call`s
an export.

## Further reading

- [`docs/RUNTIME.md`](RUNTIME.md) — GC, WasmGC refs-only, Simulation
- [`docs/SUPPORTED.md`](SUPPORTED.md) — language subset
