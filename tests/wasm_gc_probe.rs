//! Phase 4 host probe.
//!
//! Decision 3 puts wasm reclamation on the host engine: Simula objects become
//! WasmGC `struct` / `array` values and the engine traces them. Before any of
//! `src/codegen/wasm.rs` is retargeted, this asks the machine we actually test
//! on whether it speaks WasmGC at all — a module that only uses `struct.new`
//! and `struct.get`, run through the same `node` binary as `tests/mir_wasm.rs`
//! and through `wasmtime` when it is installed.
//!
//! A host without WasmGC **skips** rather than fails. The gate that turns this
//! into a hard requirement is Phase 4d ("CI installs/uses a WasmGC-capable
//! runtime"), which lands with the codegen migration, not before it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, FieldType, FunctionSection, HeapType, Instruction,
    Module, RefType, StorageType, TypeSection, ValType,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Type index of the probe struct; the func type follows it at index 1.
const STRUCT_TYPE: u32 = 0;
const FUNC_TYPE: u32 = 1;
/// The two immutable `i32` fields sum to this, so a host that instantiates the
/// module but mis-reads the fields is still caught.
const EXPECTED: i32 = 42;

/// `(type $pair (struct (field i32) (field i32)))` plus
/// `(func (export "probe") (result i32))` building one and adding its fields.
///
/// Deliberately minimal: no memory, no imports, no WASI. If a host rejects
/// this, it rejects WasmGC, not something incidental to our codegen.
fn probe_module() -> Vec<u8> {
    let mut types = TypeSection::new();
    let field = FieldType {
        element_type: StorageType::Val(ValType::I32),
        mutable: false,
    };
    types.ty().struct_([field, field]);
    types.ty().function([], [ValType::I32]);

    let mut functions = FunctionSection::new();
    functions.function(FUNC_TYPE);

    let mut exports = ExportSection::new();
    exports.export("probe", ExportKind::Func, 0);

    let pair_ref = ValType::Ref(RefType {
        nullable: true,
        heap_type: HeapType::Concrete(STRUCT_TYPE),
    });
    let mut body = wasm_encoder::Function::new([(1, pair_ref)]);
    body.instruction(&Instruction::I32Const(40));
    body.instruction(&Instruction::I32Const(2));
    body.instruction(&Instruction::StructNew(STRUCT_TYPE));
    body.instruction(&Instruction::LocalSet(0));
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::StructGet {
        struct_type_index: STRUCT_TYPE,
        field_index: 0,
    });
    body.instruction(&Instruction::LocalGet(0));
    body.instruction(&Instruction::StructGet {
        struct_type_index: STRUCT_TYPE,
        field_index: 1,
    });
    body.instruction(&Instruction::I32Add);
    body.instruction(&Instruction::End);

    let mut code = CodeSection::new();
    code.function(&body);

    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

/// What a host had to say about the probe module.
#[derive(Debug)]
enum Probe {
    /// The module ran; the payload is what `probe` returned.
    Ran(i32),
    /// The host is present but would not take the module.
    NoGcSupport(String),
    /// The host is not installed here, so it says nothing either way.
    HostMissing(String),
}

fn probe_wasm_path() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("sim-wasm-gc-probe-{id}.wasm"))
}

fn write_probe_module() -> PathBuf {
    let path = probe_wasm_path();
    std::fs::write(&path, probe_module()).expect("probe module should be writable");
    path
}

/// The `node` path `tests/mir_wasm.rs` uses, minus the WASI imports the probe
/// module does not need.
fn probe_with_node(path: &Path) -> Probe {
    let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_gc_probe.mjs");
    if !runner.exists() {
        return Probe::HostMissing(format!("missing runner {}", runner.display()));
    }
    let Ok(output) = Command::new("node").arg(&runner).arg(path).output() else {
        return Probe::HostMissing("node is not on PATH".to_string());
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if let Some(value) = stdout.strip_prefix("probe=") {
        return match value.parse::<i32>() {
            Ok(value) => Probe::Ran(value),
            Err(_) => Probe::NoGcSupport(format!("node returned a non-integer: {value:?}")),
        };
    }
    if let Some(message) = stdout.strip_prefix("unsupported=") {
        return Probe::NoGcSupport(message.to_string());
    }
    Probe::HostMissing(format!(
        "node runner said nothing usable: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    ))
}

/// Wasmtime is optional here; `tests/mir_wasm.rs` runs on Node. It is worth
/// probing anyway because Phase 4d names it as a supported host.
fn probe_with_wasmtime(path: &Path) -> Probe {
    let Ok(output) = Command::new("wasmtime")
        .arg("run")
        .arg("--invoke")
        .arg("probe")
        .arg(path)
        .output()
    else {
        return Probe::HostMissing("wasmtime is not on PATH".to_string());
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Probe::NoGcSupport(stderr.trim().to_string());
    }
    // `--invoke` prints the result on stdout; older builds add a deprecation
    // notice on stderr, which is why only stdout is parsed.
    match stdout.trim().lines().next_back().map(str::trim) {
        Some(line) => match line.parse::<i32>() {
            Ok(value) => Probe::Ran(value),
            Err(_) => Probe::NoGcSupport(format!("wasmtime returned a non-integer: {line:?}")),
        },
        None => Probe::NoGcSupport(format!(
            "wasmtime printed nothing: stderr={}",
            stderr.trim()
        )),
    }
}

fn assert_probe(host: &str, probe: Probe) {
    match probe {
        Probe::Ran(value) => assert_eq!(
            value, EXPECTED,
            "{host} instantiated the WasmGC module but `probe` returned {value}"
        ),
        Probe::NoGcSupport(message) => {
            panic!(
                "{host} is installed but rejected WasmGC ({message}). \
                 Phase 4 requires a WasmGC-capable host; see docs/RUNTIME.md."
            );
        }
        Probe::HostMissing(message) => {
            eprintln!("skipping: no {host} host to probe ({message})");
        }
    }
}

#[test]
fn the_probe_module_is_a_well_formed_wasm_binary() {
    let bytes = probe_module();
    assert!(bytes.starts_with(b"\0asm"), "expected wasm magic");
    // 0xFB is the GC instruction prefix; without it this would not be a probe.
    assert!(
        bytes.windows(2).any(|pair| pair == [0xFB, 0x00]),
        "expected a struct.new opcode in the probe module"
    );
}

#[test]
fn node_runs_a_minimal_wasm_gc_module() {
    let path = write_probe_module();
    let probe = probe_with_node(&path);
    let _ = std::fs::remove_file(&path);
    assert_probe("node", probe);
}

#[test]
fn wasmtime_runs_a_minimal_wasm_gc_module() {
    let path = write_probe_module();
    let probe = probe_with_wasmtime(&path);
    let _ = std::fs::remove_file(&path);
    assert_probe("wasmtime", probe);
}

/// Phase 4a: a class layout from `layout.rs` → WasmGC struct via
/// `codegen::wasm_gc`, then the same host runners as the minimal probe.
#[test]
fn node_runs_a_layout_mapped_point_struct() {
    use outimage::codegen::wasm_gc::{POINT_SUM_PROBE_EXPECTED, point_sum_probe_module};
    use outimage::error::Span;
    use outimage::layout::{ClassLayout, FieldLayout, FieldType};

    let layout = ClassLayout {
        name: "Point".into(),
        declared_name: "Point".into(),
        decl_span: Span::default(),
        fields: vec![
            FieldLayout {
                name: "x".into(),
                offset: 8,
                size: 8,
                ty: FieldType::I64,
                class_qual: None,
            },
            FieldLayout {
                name: "y".into(),
                offset: 16,
                size: 8,
                ty: FieldType::I64,
                class_qual: None,
            },
        ],
        methods: vec![],
        virtual_methods: vec![],
        constructor_params: vec![],
        needs_init: false,
        runs_on_own_stack: false,
        enclosing_captures: vec![],
        size: 24,
        class_id: 0,
        system_block: 0,
        prefix: None,
    };
    let bytes = point_sum_probe_module(&layout).expect("encode Point probe");
    let path = probe_wasm_path();
    std::fs::write(&path, bytes).expect("write Point probe");
    let probe = probe_with_node(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        Probe::Ran(value) => assert_eq!(
            value, POINT_SUM_PROBE_EXPECTED,
            "node ran the layout-mapped Point module but probe returned {value}"
        ),
        Probe::NoGcSupport(message) => {
            panic!(
                "node is installed but rejected the layout-mapped WasmGC module ({message}). \
                 Phase 4 requires a WasmGC-capable host; see docs/RUNTIME.md."
            );
        }
        Probe::HostMissing(message) => {
            eprintln!("skipping: no node host to probe ({message})");
        }
    }
}

/// Phase 4c: ObjectRef survives detach/resume under WasmGC via the root-handle
/// table (`CORO_ARG` / spill slots store table indices; live refs stay in the
/// eqref table the host can trace).
#[test]
fn simrt_wasm_gc_detach_preserves_object_ref() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Box; begin integer n; end;
            class Worker;
            begin
                ref(Box) b;
                b :- new Box;
                b.n := 7;
                detach;
                OutInt(b.n, 0); OutImage;
            end;
            ref(Worker) w;
            w :- new Worker;
            call(w);
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-detach-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "7",
                "detach/resume should keep Box.n under WasmGC, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected sequencing+WasmGC module ({message})")
        }
    }
}

/// Phase 4b: SysOut terminal is a pinned WasmGC ref; `sysout.OutText` uses the
/// linear image buffer after `ref.eq` against the reserved root-handle slot.
#[test]
fn simrt_wasm_gc_sysout_outtext() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            sysout.OutText("ok");
            sysout.OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-sysout-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "ok",
                "sysout.OutText under WasmGC should print ok, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => panic!("node rejected WasmGC SysOut module ({message})"),
    }
}

/// Phase 4b foothold: with WasmGC forced on, `ObjectRef` lowers to
/// `(ref null eq)` and `NewObject` / field ops use `struct.new_default` /
/// `struct.get` / `struct.set`. Free `OutText`/`OutImage` and SysOut object
/// receivers both work under this mode.
#[test]
fn simrt_wasm_gc_object_ref_new_and_fields() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Point; begin integer x, y; end;
            ref(Point) p;
            p :- new Point;
            p.x := 40; p.y := 2;
            OutInt(p.x + p.y, 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-obj-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "42",
                "WasmGC ObjectRef field lowering should print 42, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => panic!(
            "node rejected a WasmGC ObjectRef module ({message}); \
             WasmGC is required once ObjectRef lowers to host refs"
        ),
    }
}

/// WasmGC SIMSET: `linkage_base` subtyping lets SUC/PRED use shared struct fields.
#[test]
fn simrt_wasm_gc_simset_into_and_empty() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            SIMSET begin
                ref(Head) h;
                ref(Link) a, b;
                h :- new Head;
                a :- new Link;
                b :- new Link;
                a.into(h);
                b.into(h);
                if not h.empty then OutText("ok"); OutImage;
            end;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-simset-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "ok",
                "WasmGC SIMSET into/empty should print ok, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => panic!("node rejected WasmGC SIMSET module ({message})"),
    }
}

/// Phase 4-R4: `dec(r.x)` takes the address of the `ref(C)` local as the
/// thunk `env`. That home is a `ref_cell`, not a linear address, so the
/// module compiles and Jensen re-eval sees each write to `r.x`.
#[test]
fn simrt_wasm_gc_name_field_uses_ref_cell() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class C; begin integer x; end;
            ref(C) r; integer y;
            integer procedure dec(n); name n; integer n;
            begin
                if n <= 0 then dec := 0
                else begin
                    n := n - 1;
                    dec := dec(n) + 1;
                end;
            end;
            r :- new C;
            r.x := 3;
            y := dec(r.x);
            OutInt(y, 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-localaddr-obj-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "3",
                "dec(r.x) through a ref_cell should print 3, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC ref_cell name-field module ({message})")
        }
    }
}

/// simtst34 shape: outlined formal-procedure + boolean name actual.
#[test]
fn simrt_wasm_gc_formal_proc_and_boolean_name() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            Boolean bool;
            Procedure P(F, a); name F, a; Procedure F; Boolean a;
            begin
                a := not a;
                if a then P2(F) else F;
            end;
            procedure P2(F); procedure F;
            begin
                Boolean a;
                a := true;
                P(F, a);
                bool := true;
                if bool then P(Q, bool) else P(Q, bool);
            end;
            procedure Q;
            begin
            end;
            if bool then P(Q, bool) else P(Q, bool);
            Outtext("ok");
            Outimage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-fp-bool-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));
    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "ok",
                "formal-proc + boolean name, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => eprintln!("skipping WASI run: {message}"),
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected formal-proc boolean-name module ({message})")
        }
    }
}

/// simtst35 shape: type procedure used as a read-only name actual.
#[test]
fn simrt_wasm_gc_type_proc_name_actual() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            integer i, j;
            integer procedure sqri;
                sqri := i * i;
            integer procedure P(f); name f; integer f;
            begin
                i := i + 1;
                if i = 3 then P := f else P := f + P(f);
            end;
            i := 0;
            j := P(sqri);
            OutInt(j, 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-type-proc-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));
    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            // i goes 1,2,3; sqri sees those values: 1+4+9 = 14
            assert_eq!(
                stdout.trim(),
                "14",
                "P(sqri) should re-eval sqri, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => eprintln!("skipping WASI run: {message}"),
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected type-proc name-actual module ({message})")
        }
    }
}

/// Expression name actual that captures an object and a nested name formal.
#[test]
fn simrt_wasm_gc_expr_name_object_and_thunk() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class C; begin integer x; end;
            ref(C) r;
            integer y;
            integer procedure fact(n); name n; integer n;
            begin
                if n <= 1 then fact := 1 else fact := n * fact(n - 1);
            end;
            r :- new C;
            r.x := 4;
            y := fact(r.x - 1);
            OutInt(y, 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-expr-pack-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));
    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "6",
                "fact(r.x-1) with x=4 should be 3! = 6, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => eprintln!("skipping WASI run: {message}"),
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected expr-pack name-actual module ({message})")
        }
    }
}

/// Phase 4-R3: an enclosing `ref` captured by an ordinary (not own-stack) class
/// is a value snapshot kept in step by `refresh_enclosing_captures` /
/// `writeback_enclosing_captures` — no linear address in an ObjectRef slot.
/// The class both reads the captured `ref` and rebinds it, and the caller must
/// see the rebinding after the method returns.
#[test]
fn simrt_wasm_gc_enclosing_object_ref_capture_round_trip() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Box; begin integer n; end;
            ref(Box) shared;
            class User;
            begin
                procedure bump;
                begin
                    shared.n := shared.n + 1;
                    shared :- new Box;
                    shared.n := 9;
                end;
            end;
            ref(User) u;
            shared :- new Box;
            shared.n := 1;
            u :- new User;
            u.bump;
            OutInt(shared.n, 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-encl-ref-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "9",
                "the enclosing `ref` rebound inside the method should be visible \
                 to the caller, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected the WasmGC enclosing-ref capture module ({message})")
        }
    }
}

/// Type-section foothold still appends the registry when forced on (even if
/// code size can shrink because bump `NewObject` sequences go away).
#[test]
fn simrt_wasm_gc_env_appends_registry_types() {
    use outimage::codegen::wasm_gc::{self, GcTypeRegistry};

    let mut reg = GcTypeRegistry::new();
    let _ = reg.spill_refs_array();
    let _ = reg.text_frame();
    assert!(reg.len() >= 2);

    let source = r#"
        begin
            class Point; begin integer x, y; end;
            ref(Point) p;
            p :- new Point;
            OutText("ok"); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-env-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let with_gc = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(with_gc.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(stdout.trim(), "ok");
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => panic!(
            "node rejected a WasmGC-typed module ({message}); \
             WasmGC is required once the type section is present"
        ),
    }
}

/// Workstream 2: `Text` and `ArrayI64` both lower to WasmGC refs — `text_frame`
/// struct over a shared `(array i8)`, integer array as a `{ elems, ndims,
/// bounds }` descriptor over `(array i64)`. Exercises literal-to-frame
/// construction (`t :- "hi"`), array allocation/store/load, and the host
/// bridge that copies a GC `text_frame`'s chars into linear memory for
/// `OutText`.
#[test]
fn simrt_wasm_gc_text_and_array_i64() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
                    text t; integer array a(1:3);
            t :- "hi"; a(1):=10; a(2):=20; a(3):=30;
            OutText(t); OutInt(a(1)+a(2)+a(3), 0); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-text-array-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "hi60",
                "WasmGC Text+ArrayI64 lowering should print hi60, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC Text/ArrayI64 module ({message})")
        }
    }
}

/// Phase 4-R4: `ref(T) array` elements are a direct `(array (mut (ref null
/// eq)))` spine on the `array_object` descriptor — no root handles. Covers
/// store/load, `none` round-tripping, and passing the descriptor to a
/// procedure (which sees it through the `(ref null $array_i64)` parameter
/// type every `MirType::ArrayI64` maps to, so the subtype cast must hold).
#[test]
fn simrt_wasm_gc_object_ref_array_elements() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Cell(v); integer v; begin end;
            ref(Cell) array a(1:3);
            integer i;
            procedure bump(p); ref(Cell) array p;
                p(3) :- new Cell(7);
            for i := 1 step 1 until 2 do a(i) :- new Cell(i);
            bump(a);
            a(2) :- none;
            if a(2) == none then OutInt(a(1).v + a(3).v, 0);
            OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-ref-array-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "8",
                "ref(T) array elements should round-trip as direct eqrefs, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC ref(T)-array module ({message})")
        }
    }
}

/// Workstream 2: `text.content_eq` under WasmGC copies both frames to linear
/// scratch and compares bytes — the `=`/`=/=` value comparison on `text`.
#[test]
fn simrt_wasm_gc_text_content_eq() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            text a, b, c;
            a :- "hello";
            b :- "hello";
            c :- "world";
            if a = b then OutText("ab") else OutText("!ab");
            if a <> c then OutText(" ac") else OutText("!ac");
            OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-text-content-eq-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "ab ac",
                "WasmGC text content equality should print ab ac, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC TextContentEq module ({message})")
        }
    }
}

/// Workstream 2 TEXTOBJ-share smoke test: `t.sub(i, n)` under WasmGC returns a
/// fresh `text_frame` that shares `t`'s `chars` array ref rather than copying
/// bytes. Two independently constructed sub-views of the same `t` at the same
/// `(i, n)` therefore compare `==` (view equality — same `chars`/`start`/
/// `length`), which [`emit_text_ref_eq_gc`] checks directly against the
/// struct fields instead of scanning linear memory.
#[test]
fn simrt_wasm_gc_text_sub_shares_chars() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            text t, s;
            t :- "hi";
            s :- t.sub(1, 1);
            if s == t.sub(1, 1) then OutText("eq") else OutText("ne");
            OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-text-sub-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "eq",
                "WasmGC t.sub(1,1) TEXTOBJ-share check should print eq, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC Text.sub module ({message})")
        }
    }
}

fn compile_wasm_node(source: &str, output_path: &Path) -> Vec<u8> {
    let _ = std::fs::remove_file(output_path);
    match outimage::compile_with_options(
        &outimage::source::SourceFile::anonymous(source),
        &outimage::CompileOptions::for_compile(
            output_path.to_path_buf(),
            outimage::CompileTarget::WasmNode,
        ),
    )
    .unwrap_or_else(|error| panic!("wasm compile failed: {error}"))
    {
        outimage::CompileResult::Artifact(path) => {
            std::fs::read(&path).expect("read wasm artifact")
        }
        outimage::CompileResult::Interpreted(_) | outimage::CompileResult::Checked => {
            panic!("expected wasm artifact")
        }
    }
}

/// Outcome of running a full WASI module under Node (stdout captured on success).
#[derive(Debug)]
enum WasiProbe {
    Ran(String),
    NoGcSupport(String),
    HostMissing(String),
}

/// Run a full WASI module and return stdout on success.
fn probe_compiled_wasi_stdout(path: &Path) -> WasiProbe {
    let runner = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run_wasi.mjs");
    if !runner.exists() {
        return WasiProbe::HostMissing(format!("missing runner {}", runner.display()));
    }
    let Ok(output) = Command::new("node").arg(&runner).arg(path).output() else {
        return WasiProbe::HostMissing("node is not on PATH".to_string());
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        return WasiProbe::Ran(stdout.to_string());
    }
    let combined = format!("{stdout}\n{stderr}");
    if combined.contains("WasmGC")
        || combined.contains("struct")
        || combined.contains("invalid")
        || combined.contains("CompileError")
        || combined.contains("TypeError")
        || combined.contains("WebAssembly")
    {
        return WasiProbe::NoGcSupport(combined.trim().to_string());
    }
    WasiProbe::NoGcSupport(format!(
        "run_wasi failed: status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    ))
}

/// Phase 4-R4: class Text / ObjectRef attributes are typed WasmGC fields,
/// not i64 root-handles. A `ref` attribute and a `text` attribute must
/// round-trip without the handle table.
#[test]
fn simrt_wasm_gc_class_text_array_object_attrs() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Box;
            begin
                ref(Box) next;
                text t;
            end;
            ref(Box) p, q;
            p :- new Box;
            q :- new Box;
            p.next :- q;
            p.t :- "hi";
            OutText(p.t);
            if p.next == q then OutText("ok");
            OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-class-attrs-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));

    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "hiok",
                "typed class Text/ObjectRef attrs should print hiok, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC class-attr module ({message})")
        }
    }
}

/// Phase 4-R4: the shared root-handle table is gone. A compiled module has
/// the funcref table plus the BASICIO pinning table — never a third eqref
/// table that `table.grow`s on every ObjectRef store.
#[test]
fn simrt_wasm_gc_has_no_root_handle_table() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            class Box; begin ref(Box) next; text t; end;
            ref(Box) p;
            p :- new Box;
            p.t :- "x";
            OutText(p.t); OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-no-handle-table-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    let _ = std::fs::remove_file(&path);
    let tables = wasm_table_section_count(&bytes);
    assert_eq!(
        tables, 2,
        "expected funcref + BASICIO tables only, found {tables} table(s)"
    );
}

/// Phase 4-R4: an own-stack class shares an enclosing `ref` through a
/// `ref_cell`. Detach/resume must see the same object, not a snapshot.
#[test]
fn simrt_wasm_gc_own_stack_ref_cell_capture() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            ref(Worker) w;
            ref(Box) shared;
            class Box; begin integer n; end;
            class Worker;
            begin
                shared :- new Box;
                shared.n := 1;
                detach;
                shared.n := shared.n + 1;
            end;
            shared :- none;
            w :- new Worker;
            if shared =/= none then OutInt(shared.n, 0);
            resume(w);
            if shared =/= none then OutInt(shared.n, 0);
            OutImage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-own-stack-refcell-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));
    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "12",
                "own-stack ref_cell capture should print 12, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC own-stack ref_cell module ({message})")
        }
    }
}

/// Phase 4-R4 / simtst96: `link` siblings `town` (Text `nam_` at offset 24)
/// and `townpoint` (`ref(town) t` at the same offset) must share a WasmGC
/// ancestor so `t.nam_` does not `ref.cast` a townpoint to `town`.
#[test]
fn simrt_wasm_gc_linkage_sibling_text_and_ref_at_same_offset() {
    use outimage::codegen::wasm_gc;

    let source = r#"
        begin
            link class town(nam_); value nam_; text nam_;
            begin
            end;
            link class townpoint(t); ref(town) t;
            begin
                procedure write;
                begin
                    outtext(t.nam_);
                end;
            end;
            ref(head) h;
            ref(town) r;
            h :- new head;
            r :- new town("SAND");
            new townpoint(r).into(h);
            h.first qua townpoint.write;
            outimage;
        end;
    "#;
    let path = std::env::temp_dir().join(format!(
        "sim-wasm-gc-linkage-siblings-{}.wasm",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = wasm_gc::with_force_enabled(true, || compile_wasm_node(source, &path));
    assert!(bytes.starts_with(b"\0asm"));
    let probe = probe_compiled_wasi_stdout(&path);
    let _ = std::fs::remove_file(&path);
    match probe {
        WasiProbe::Ran(stdout) => {
            assert_eq!(
                stdout.trim(),
                "SAND",
                "townpoint.t.nam_ through a link sibling should print SAND, got {stdout:?}"
            );
        }
        WasiProbe::HostMissing(message) => {
            eprintln!("skipping WASI run: {message}");
        }
        WasiProbe::NoGcSupport(message) => {
            panic!("node rejected a WasmGC linkage-sibling module ({message})")
        }
    }
}

/// Count tables in a Wasm binary's table section (id 4).
fn wasm_table_section_count(bytes: &[u8]) -> u32 {
    assert!(bytes.starts_with(b"\0asm"), "not a wasm binary");
    let mut i = 8;
    while i < bytes.len() {
        let id = bytes[i];
        i += 1;
        let (size, n) = read_leb128(&bytes[i..]);
        i += n;
        if id == 4 {
            let (count, _) = read_leb128(&bytes[i..]);
            return count;
        }
        i += size as usize;
    }
    0
}

fn read_leb128(bytes: &[u8]) -> (u32, usize) {
    let mut result = 0u32;
    let mut shift = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (result, index + 1);
        }
        shift += 7;
        if shift > 28 {
            break;
        }
    }
    (result, bytes.len())
}
