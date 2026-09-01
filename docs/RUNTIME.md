# Runtime model

## Memory

The **MIR interpreter** has a mark-sweep collector with free-list slot reuse
(`src/mir/interp/gc.rs`). Tracing is precise via per-word `SlotTag`s written at
store sites. Collection runs at safepoints between ops (default every 1024
object/array allocations). Extensions: `SIM_GC_STATS=1` (stderr summary,
including `pause_ns`), `SIM_GC_EVERY=N` (override threshold; `0` disables;
`1` stress).

**Native** has a mark-sweep collector over a managed heap (`runtime/gc.c`,
`runtime/gc.h`). Class instances, all three array flavours, text frames, and
text objects are allocated by `simrt_gc_alloc` with an eight-byte-aligned
header (`next`, `kind`, `flags`, `size`); `size` is the *last* header member,
so the payload size still sits at `obj - 8` where object field bounds checks
expect it.

Frame roots are **precise**. Cranelift emits a linked list of root frames
(`simrt_gc_root_push` / `_pop`) whose slots hold every GC-typed MIR local
(`ObjectRef`, `Text`, `Array*`, `RefI64`). A parked coroutine saves that list
head on switch. Collection walks the running list plus every parked head, then
the explicit C-runtime roots:

- `SYSIN` / `SYSOUT`, BASICIO file objects, the SQS
  (`simrt_gc_visit_runtime_roots`)
- every registered quasi-parallel component's object
  (`simrt_seq_gc_visit_roots`)

Slots hold pointer values, including `Op::FieldAddr` interiors (`object +
offset`); those pin their block via `simrt_gc_block_at`. An integer that
merely looks like a heap address is not a root. Heap tracing is still
conservative for objects and int64/ref arrays, precise for text-array element
slots, and skipped for real arrays.

One deliberate retention remains: a component's object is a permanent root,
matching the interpreter's `SeqComponent` retention.

Native collection is **on by default**, every 1024 allocations, the same
threshold the interpreter uses. `SIM_GC_EVERY=N` overrides it — `0` disables
automatic collection (the managed heap and explicit `simrt_gc_collect()`
stay), `1` collects on every allocation. Runtime helpers that allocate more
than once call `simrt_gc_defer_collect` so an in-progress C local is not
swept. The sweep does not hand blocks back to the host: it keeps them on
an **exact-size free list** that `simrt_gc_alloc` draws from first, so a
loop churning one shape of object keeps landing in the slot it just vacated
(`slots_reused` in the stats line). The list is bounded — the search gives up
after 64 candidates and the sweep frees to the host past 4096 held blocks — so
sizes that never repeat cannot accumulate.

Interp/native collectors are **non-moving mark-sweep**. Compaction and
generational collection are deliberate non-goals for those backends; wasm
already gets whatever strategy the host WasmGC engine uses.

Text frames and text objects **are swept** (Phase 3 step 2): character storage
is folded into the same managed `TEXT_OBJECT` block as the header, so interior
`simrt_text_content_ptr` values pin their object via the collector's address
range check. `SIM_GC_STATS=1` prints one summary line to **stderr** at exit
— never stdout — including `pause_ns` (time spent inside the collector). Wasm
has no in-module collector, so that counter is N/A there (host engines may
expose their own). Collection is disabled outright on the `simrt_error`
path. No collection has any observable effect: no finalizers, and a file object
is never closed by the collector.

**Wasm** uses **host WasmGC only** for Simula objects / texts / arrays (no
bump-heap object mode, no in-module collector). Linear memory remains for
WASI/IO scratch and scalar spill. Interpreter and native keep their own
collectors.

**Refs-only WasmGC** is the live architecture. There is no root-handle table.
SIMSET `SUC`/`PRED`
are eqref fields; parked-frame refs live in an `(array eqref)` spine;
address-taken ObjectRef locals are `ref_cell` homes.

Progress:

- **4a** — `src/codegen/wasm_gc.rs` maps layouts to WasmGC types.
- **4c / 4-R2** — dual spill shape; ref spine is `(array eqref)` per
  coroutine; SIMSET `SUC`/`PRED` are direct eqrefs.
- **4-R3** — ordinary enclosing ObjectRef captures are value snapshots.
  Address-taken ObjectRef locals are `ref_cell` homes.
- **4-R4 landed** — `ROOT_HANDLE_TABLE` is gone. Class Text/Array/ObjectRef
  attrs are typed WasmGC fields. Linkage trailing refs are uniform eqref,
  and sibling ring classes that share an offset (`town.nam_` vs
  `townpoint.t`) hang off a shared eqref ladder so `ref.cast` names an
  ancestor both satisfy. Own-stack by-ref `ref` captures and name-thunk
  `env`s are GC objects. SIMULATION's MAIN is a header-only GC object, so the
  three process slots and the SQS process column stay reference-typed end to
  end. A reference has **no** integer encoding: nothing converts between an
  eqref and an `i64` word any more, and a MIR `Copy` / compare / call / return
  that mixes the two is a codegen error rather than a bridge. BASICIO:
  `sysin` / `sysout` live in two mutable eqref globals; a disk file's host
  identity is its slot in a BASICIO-only pinning table, which is the module's
  only non-funcref table.

### Wasm host requirements (Phase 4d)

Live DosTest / `run_wasi.mjs` modules **always** emit WasmGC types. Hosts
without WasmGC fail clearly (no in-module collector fallback).

The shared root-handle table is gone (4-R4); the module's tables are the
funcref table and the BASICIO pinning table. Host requirements:

| Host | Notes |
| --- | --- |
| Node.js | V8 WasmGC — use a current Node (22+ recommended); probe via `tests/fixtures/run_gc_probe.mjs` |
| Wasmtime | Build/run with GC support; `tests/wasm_gc_probe.rs` exercises `wasmtime run --invoke probe` |
| Browsers | Chromium / Firefox / Safari engines already ship WasmGC |

`tests/wasm_gc_probe.rs` probes whether the local host can run a minimal
WasmGC module (and a layout-mapped Point struct), and runs a Simula
`new Point` + field program under `with_force_enabled`. If Node or Wasmtime
**is** installed, lack of WasmGC is a hard test failure; if the host binary is
missing, the probe skips. There is **no** in-module collector fallback.

Interpreter and native collectors are in place (including precise native
root frames); wasm is refs-only (4-R1…R5).

Artificial process-wide soft caps (`MAX_OBJECTS` / `MAX_ARRAYS` / `MAX_TEXTS`)
are **not** used: allocation is bounded by host memory, with clean OOM
diagnostics.

## Failures

User-facing Standard failures should prefer `simrt_error(message)` which
prints `sim: …` to stderr and **`exit(1)`**. Internal invariant
violations may still `abort()`.

## Simulation

SQS state is process-global C state (`simrt_sim_*`) on native; the MIR
interpreter keeps an equivalent `Vec` in `Vm` sequencing state; wasm lowers
SQS through `src/mir/sim_runtime.rs` (asyncify). Nested Simulation blocks are
rejected on all backends.

Scheduling is **deterministic**: activation order uses event time plus a
monotonic sequence number (no wall-clock). Re-running the same program yields
the same SQS order. ENVIRONMENT random streams are separate and seeded by the
program's own `name` integer actuals (e.g. `U := 1; draw(0.5, U)`).

Documented quantitative limits (§0.5.2):

- SQS length: at most 65 536 event notices (`SIMRT_RT_SQS_MAX_LEN` /
  `simulation::MAX_SQS_LENGTH` / wasm `SQS_MAX_LEN`), enforced on **interp,
  native, and wasm** with the message `SQS length limit exceeded`.
- Native BASICIO file registry: 64 slots (`SIMRT_BASICIO_MAX_FILES`), with
  retired-slot reuse on close. Exceeding it reports `too many open files`.
- Native coroutine stacks: 512 KiB usable plus a guard page, override with
  `SIMRT_CORO_STACK_KB` (16..2²⁰). Overflow of the usable stack faults on
  the guard page rather than scribbling on another component.
- Quasi-parallel systems: 256 simultaneously active
  (`SIMRT_SEQ_MAX_SYSTEMS`).

There is no `MAX_SUSPEND_DEPTH` check. Nested `call`/`resume` depth is bounded
by the coroutine stack above (native) or the host stack (interp / wasm).

SIMSET ring links use fixed object offsets (`SUC`/`PRED` after `class_id`).
`simrt_simset_*` implements into/out/precede/follow/suc/pred/empty/cardinal;
`wait(head)` is `into` + `passivate` in Process bodies.

Enclosing ObjectRef names used from a class body (e.g. `wait(q)`) are stored
on the instance at `new` (same snapshot timing as the interpreter's
`enclosing_locals`).

Coroutine resume state for Process-prefixed objects is stored in the
`__simrt_coro_pc` field. Native `-g` builds expose that member in DWARF class
structures so debuggers can inspect the resume PC (multi-stack UX MVP).

## BASICIO

Terminal SysOut uses a 1-based image buffer (`OutText` / `OutChar` write at
`pos`; `OutImage` flushes the full image + newline; `BreakOutImage` flushes
characters `1..pos-1`). SysIn has a parallel image for `InImage` / `InChar` /
`Endfile`. Free `InLine` still returns a fresh text without using the SysIn
image.

`InFile` / `OutFile` / `PrintFile` / `DirectFile` / bytefile classes follow
Standard Chapter 10 shapes:

```
outf :- new OutFile(filename);
if outf.open(blanks(n)) then begin … outf.outint(i, w); outf.close; end;
```

`FILENAME` is the constructor parameter; image `open` takes the buffer and
returns Boolean. Bytefiles use parameterless `open`. Free `OutInt(i, w)`
follows §10.5.8 (both arguments required; `w = 0` is exact width). Interpreter
covers item I/O, PrintFile pagination, direct/byte files, and
`terminate_program`. Native covers image InFile/OutFile, sequential
InByteFile/OutByteFile, DirectFile (`locate` / `inimage` / `outimage` /
`lastloc`), and `terminate_program`. SYSIN/SYSOUT are opened with
`blanks(linelength)` at start and closed at program STOP. Free identifiers
such as `OutText`, `line`, and `eject` resolve as under the Standard embedding
`inspect SYSIN do inspect SYSOUT do` (SYSOUT innermost for shared names). Wasm
is terminal free wrappers only.
