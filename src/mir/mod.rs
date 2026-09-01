//! Mid-level IR (MIR): a scalar-oriented intermediate representation that
//! sits between the (semantically-checked) AST and backends.
//!
//! The MIR is deliberately simple and *not* SSA:
//! each [`Function`] owns a flat vector of [`Local`] storage slots addressed
//! by [`LocalId`], and a vector of [`BasicBlock`]s addressed by [`BlockId`].
//! Locals are plain mutable storage cells (no phi nodes); branches and jumps
//! only affect control flow, values are read/written through
//! [`Op::StoreLocal`] and ordinary reads.
//!
//! [`lower::lower_program`] builds a [`Module`] from `ast::Program` for the
//! scalar subset; unsupported constructs are a hard
//! error (multi-dimensional arrays, objects, `for`, `goto`, simulation
//! statements, …). Simple nested (local) procedures with call-by-value
//! parameters and an optional integer/boolean result are supported: each one
//! lowers to its own [`Function`] alongside `main`.

pub mod asyncify;
pub mod build;
pub mod foreign;
pub mod interp;
pub mod link;
pub mod lower;
pub mod ref_cell;
pub mod seq_runtime;
pub mod sim_runtime;

use std::fmt;

pub use crate::error::Span;
pub use foreign::{
    ForeignAbi, ForeignConv, ForeignKind, ForeignType, native_export_symbol,
    parse_export_identification,
};
pub use link::merge_modules;
pub use lower::{lower_program, lower_program_lenient, lower_program_with_source};

use crate::layout::ClassLayout;
use crate::target::Charset;

/// A compiled module: functions plus a shared string pool.
#[derive(Debug, Clone, Default)]
pub struct Module {
    pub functions: Vec<Function>,
    /// String pool referenced by [`Op::CallOutText`], [`Op::TextFromLiteral`],
    /// and other text ops.
    pub strings: Vec<String>,
    /// Class layouts used by object ops and by `-g` DWARF structure types.
    pub class_layouts: Vec<ClassLayout>,
    /// `external procedure` declarations that no compiled module supplied a
    /// body for, lowered as empty stubs. Checking one module in isolation is
    /// legitimate, so these are only fatal when producing an artifact — see
    /// [`Module::ensure_externals_resolved`]. Foreign (`C` / `JS` / `Host`)
    /// stubs are bound by the backend and are not listed here.
    pub unresolved_externals: Vec<UnresolvedExternal>,
    /// C/JS text-copy encoding (`--charset`). Interpreter Host clones frames
    /// without encoding.
    pub charset: Charset,
}

/// A Simula-kind `external procedure` waiting for a providing module.
#[derive(Debug, Clone)]
pub struct UnresolvedExternal {
    pub name: String,
    /// Identification string (`= "utils"`), naming the providing module.
    pub providing_module: Option<String>,
    pub span: Span,
}

impl Module {
    /// Rejects unresolved `external procedure` stubs. Chapter 6 makes an
    /// external declaration "a substitute for a complete introduction of the
    /// corresponding source module", so a missing body is an error rather than
    /// a procedure that silently does nothing. The one empty-body form the
    /// Standard sanctions is `external <kind> procedure ... is ...`, whose body
    /// comes from a separately compiled non-Simula module.
    pub fn ensure_externals_resolved(&self) -> Result<(), crate::error::CompileError> {
        let Some(unresolved) = self.unresolved_externals.first() else {
            return Ok(());
        };
        let name = &unresolved.name;
        let span = &unresolved.span;
        let message = format!(
            "external procedure '{name}' was declared but no compiled module supplies its \
             body; pass the module that defines it as an additional source, or use \
             `sim check` to check this module on its own"
        );
        Err(if span.is_empty() {
            crate::error::CompileError::codegen(message)
        } else {
            crate::error::CompileError::codegen_at(message, span.clone())
        })
    }
    /// Renders the module as human-readable MIR text, e.g. for `--emit=mir`.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        if !self.strings.is_empty() {
            out.push_str("strings:\n");
            for (id, value) in self.strings.iter().enumerate() {
                out.push_str(&format!("  {id}: {value:?}\n"));
            }
            out.push('\n');
        }
        for (index, function) in self.functions.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(&function.dump());
        }
        out
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.dump())
    }
}

/// A single function: either the implicit `main` produced from the
/// top-level block's statements, or a simple nested (local) procedure with
/// value parameters and an optional integer/boolean result (see
/// [`lower::lower_program`]).
#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Local>,
    pub locals: Vec<Local>,
    pub entry: BlockId,
    pub blocks: Vec<BasicBlock>,
    /// Statement labels in this function (last occurrence wins), used to
    /// resolve [`Op::GotoEscape`] at runtime (§5.4.18).
    pub labels: std::collections::HashMap<String, BlockId>,
    /// `Some(ty)` for a function procedure (e.g. `integer procedure f(...)`);
    /// `None` for `main` and for void procedures. Drives both the Cranelift
    /// function signature's return type and how `Op::Return` is emitted.
    pub result: Option<MirType>,
    /// For [`MirType::ArrayI64`]/`ArrayF64`/`ArrayText` locals, the actual
    /// declared element type — e.g. `ref(Point) array` lowers to a plain
    /// `ArrayI64` descriptor local (bump mode stores an `ObjectRef` pointer
    /// as a plain `i64` word, same representation as an integer array), so
    /// under WasmGC codegen needs this to tell an integer array's `(array
    /// (mut i64))` elems from a reference array's `(array (mut anyref))`
    /// elems (see `array_gc_type_info` in `codegen::wasm`). Absent (or
    /// `MirType::I64`/`F64`/`Text`) entries mean "plain" elements; runtime
    /// helper functions built directly via `FunctionBuilder`/literal
    /// `Function` (never touch `ref(...) array`s) always leave this empty.
    pub array_elem_kinds: std::collections::HashMap<LocalId, MirType>,
    /// When `Some`, this function is a foreign import thunk.
    /// The Simula body is a placeholder; backends bind [`ForeignAbi::ident`].
    pub foreign: Option<ForeignAbi>,
    /// Public export identity: the Simula name (`add`) or `export:tick`.
    pub export: Option<String>,
    /// Flattened debug scopes recorded while lowering this function (inlined
    /// procedures and nested/prefixed blocks). Compile-time DAP metadata only:
    /// the interpreter does not read this on `sim run`.
    pub debug_scopes: Vec<DebugScope>,
}

/// Kind of a flattened debug scope inside a MIR function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugScopeKind {
    /// An inlined procedure activation.
    Procedure,
    /// A nested `begin`…`end` or prefixed block with its own declarations.
    Block,
}

/// Source range of a flattened Simula scope, used by DAP to build synthetic
/// stack frames and hide out-of-scope locals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugScope {
    pub name: String,
    pub span: Span,
    pub kind: DebugScopeKind,
}

impl Function {
    /// Resolves a [`LocalId`] to its declaration. Parameters occupy the low
    /// `LocalId`s, followed by `locals`, matching how the lowerer allocates them.
    pub fn local(&self, id: LocalId) -> &Local {
        if id.0 < self.params.len() {
            &self.params[id.0]
        } else {
            &self.locals[id.0 - self.params.len()]
        }
    }

    /// Element type of an `ArrayI64`/`ArrayF64`/`ArrayText` descriptor local
    /// `id` — from `array_elem_kinds` if a `ref(...) array`/mixed case was
    /// recorded, else the "plain" element type implied by `id`'s own array
    /// type (`I64` for `ArrayI64`, `F64` for `ArrayF64`, `Text` for
    /// `ArrayText`). See [`Function::array_elem_kinds`] for why `ArrayI64`
    /// alone isn't enough to know this.
    pub fn array_elem_ty(&self, id: LocalId) -> MirType {
        if let Some(&ty) = self.array_elem_kinds.get(&id) {
            return ty;
        }
        match self.local(id).ty {
            MirType::ArrayF64 => MirType::F64,
            MirType::ArrayText => MirType::Text,
            _ => MirType::I64,
        }
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0]
    }

    /// Whether this outlined procedure may be published as a C/Host wrapper.
    pub fn is_scalar_exportable(&self) -> bool {
        if self.name == "main" || self.foreign.is_some() {
            return false;
        }
        if self.name.contains('$') || self.name.starts_with("__simrt_") {
            return false;
        }
        let scalar = |ty: MirType| {
            matches!(
                ty,
                MirType::I64 | MirType::F64 | MirType::LongF64 | MirType::Bool
            )
        };
        self.params.iter().all(|param| scalar(param.ty)) && self.result.map(scalar).unwrap_or(true)
    }

    /// Wasm export name (raw, no `sim_` prefix).
    pub fn wasm_export_name(&self) -> Option<String> {
        let stored = self.export.as_deref()?;
        Some(
            parse_export_identification(stored)
                .unwrap_or(stored)
                .to_string(),
        )
    }

    /// Native C symbol: `sim_add`, or the exact name from `export:step`.
    pub fn native_export_name(&self) -> Option<String> {
        let stored = self.export.as_deref()?;
        if let Some(name) = parse_export_identification(stored) {
            Some(name.to_string())
        } else {
            Some(native_export_symbol(stored))
        }
    }

    pub fn dump(&self) -> String {
        let mut out = String::new();
        out.push_str("fn ");
        out.push_str(&self.name);
        out.push('(');
        for (index, param) in self.params.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}: {}", param.name, param.ty));
        }
        out.push(')');
        if let Some(result) = self.result {
            out.push_str(&format!(" -> {result}"));
        }
        if let Some(foreign) = &self.foreign {
            out.push_str(&format!(" foreign {} \"{}\"", foreign.kind, foreign.ident));
        }
        if let Some(export) = &self.export {
            out.push_str(&format!(" export \"{export}\""));
        }
        out.push_str(" {\n");

        if !self.locals.is_empty() {
            out.push_str("  locals:\n");
            for (index, local) in self.locals.iter().enumerate() {
                let id = LocalId(self.params.len() + index);
                out.push_str(&format!("    {id} {}: {}\n", local.name, local.ty));
            }
        }

        for block in &self.blocks {
            let marker = if block.id == self.entry {
                " (entry)"
            } else {
                ""
            };
            out.push_str(&format!("  {}:{}\n", block.id, marker));
            for spanned in &block.ops {
                out.push_str(&format!("    {}\n", spanned.op));
            }
        }

        out.push_str("}\n");
        out
    }
}

/// A named storage slot (declared variable or compiler-generated temporary).
#[derive(Debug, Clone)]
pub struct Local {
    pub name: String,
    pub ty: MirType,
    /// When `ty` is [`MirType::ObjectRef`], the declared class qualification
    /// (`ref(Point)` → `"Point"`) for DWARF pointer-to-struct typing.
    pub class_qual: Option<String>,
    /// Innermost flattened debug scope that owns this slot (inlined procedure
    /// or nested/prefixed block). DAP shows the local only while the PC sits
    /// inside this span. `None` means always visible in this MIR frame.
    /// Compile-time metadata only: the interpreter does not read this on
    /// `sim run`.
    pub debug_scope: Option<Span>,
}

impl Local {
    pub fn new(name: impl Into<String>, ty: MirType) -> Self {
        Self {
            name: name.into(),
            ty,
            class_qual: None,
            debug_scope: None,
        }
    }
}

/// MIR value types. Scalars (Phase 1), integer/text arrays and `text`
/// (Phase 4); object references (Phase 5 MVP); `real`,
/// `character`, and other array element types are still deferred.
///
/// `ArrayI64` / `ArrayText` locals hold an opaque pointer to a runtime-allocated
/// array descriptor (see `runtime/runtime.c`), not the array's elements
/// directly; [`Op::AllocArray`]/[`Op::ArrayLoad`]/[`Op::ArrayStore`]
/// are the only ops that touch that pointer's contents.
///
/// `Text` locals hold an opaque pointer to a runtime-allocated
/// `SimrtTextFrame` (see `runtime/runtime.c`); text ops create, copy,
/// concatenate, assign into, and print through that descriptor.
///
/// `ObjectRef` locals hold an opaque pointer to a runtime-allocated object
/// (or null for `none`); [`Op::NewObject`], [`Op::FieldLoadI64`], and
/// [`Op::FieldStoreI64`] allocate and touch fields. Reference assignment
/// (`:-`) is a plain [`Op::StoreLocal`] of the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MirType {
    I64,
    Bool,
    F64,
    /// `long real` — same IEEE-754 binary64 storage as [`Self::F64`] on this
    /// platform, but a distinct MIR type so locals/params keep the Simula
    /// distinction through codegen.
    LongF64,
    ArrayI64,
    ArrayF64,
    ArrayText,
    Text,
    ObjectRef,
    /// Pointer to an `i64` stack/heap cell (used as thunk `env` and by
    /// shared name-param get/set helpers).
    RefI64,
    /// Function pointer (table index on wasm; absolute address on native).
    FuncRef,
}

impl MirType {
    /// IEEE-754 binary64 float types (`real` / `long real`).
    pub fn is_float(self) -> bool {
        matches!(self, Self::F64 | Self::LongF64)
    }
}

impl fmt::Display for MirType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I64 => write!(f, "i64"),
            Self::Bool => write!(f, "bool"),
            Self::F64 => write!(f, "f64"),
            Self::LongF64 => write!(f, "long.f64"),
            Self::ArrayI64 => write!(f, "array.i64"),
            Self::ArrayF64 => write!(f, "array.f64"),
            Self::ArrayText => write!(f, "array.text"),
            Self::Text => write!(f, "text"),
            Self::ObjectRef => write!(f, "object"),
            Self::RefI64 => write!(f, "ref.i64"),
            Self::FuncRef => write!(f, "funcref"),
        }
    }
}

/// Identifies a [`Local`] within a [`Function`] (see [`Function::local`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub usize);

impl fmt::Display for LocalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Identifies a [`BasicBlock`] within a [`Function`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub usize);

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    /// Block parameters; unused by the current (non-SSA) lowerer, reserved
    /// for a future SSA-with-block-params form.
    pub params: Vec<LocalId>,
    pub ops: Vec<SpannedOp>,
}

/// An [`Op`] paired with the source span it was lowered from. Synthetic
/// control-flow scaffolding that the lowerer inserts itself (e.g. the jump
/// back to a `while` header) has no single corresponding source token and
/// uses the enclosing statement's span, or `0..0` when nothing is closer.
#[derive(Debug, Clone)]
pub struct SpannedOp {
    pub op: Op,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    /// Real exponentiation (`**`); always on `f64` operands.
    Pow,
    And,
    Or,
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::IntDiv => "idiv",
            Self::Pow => "pow",
            Self::And => "and",
            Self::Or => "or",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Neg => "neg",
            Self::Not => "not",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl fmt::Display for CmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSig {
    pub params: Vec<MirType>,
    pub result: Option<MirType>,
}

#[derive(Debug, Clone)]
pub enum Op {
    ConstI64 {
        dest: LocalId,
        value: i64,
    },
    /// IEEE-754 `f64` constant.
    ConstF64 {
        dest: LocalId,
        value: f64,
    },
    ConstBool {
        dest: LocalId,
        value: bool,
    },
    /// Convert a signed `i64` to IEEE-754 `f64` (Simula integer→real promotion).
    I64ToF64 {
        dest: LocalId,
        src: LocalId,
    },
    /// Convert IEEE-754 `f64` to signed `i64` via `entier` (Simula real→integer).
    F64ToI64 {
        dest: LocalId,
        src: LocalId,
    },
    Copy {
        dest: LocalId,
        src: LocalId,
    },
    Binary {
        dest: LocalId,
        op: BinOp,
        left: LocalId,
        right: LocalId,
    },
    Unary {
        dest: LocalId,
        op: UnOp,
        src: LocalId,
    },
    LoadLocal {
        dest: LocalId,
        local: LocalId,
    },
    StoreLocal {
        local: LocalId,
        src: LocalId,
    },
    /// Address of an `i64` local's stack cell ([`MirType::RefI64`] result).
    LocalAddr {
        dest: LocalId,
        local: LocalId,
    },
    /// Load `i64` through a [`MirType::RefI64`] pointer at byte `offset`.
    LoadRefI64 {
        dest: LocalId,
        ptr: LocalId,
        offset: i64,
    },
    /// Store `i64` through a [`MirType::RefI64`] pointer at byte `offset`.
    StoreRefI64 {
        ptr: LocalId,
        src: LocalId,
        offset: i64,
    },
    /// Address of the field at byte `offset` of an object reference
    /// ([`MirType::RefI64`] result), for a capture that points at an enclosing
    /// variable living in another object rather than a stack frame.
    FieldAddr {
        dest: LocalId,
        object: LocalId,
        offset: i64,
    },
    /// Allocates `bytes` contiguous bytes, live for the enclosing call site,
    /// and stores the base address in `dest` (a [`MirType::RefI64`] local).
    /// Used to build multi-field name-thunk `env` cells (e.g. the packed
    /// `(array, index_ptr)` pair for an integer array-element name actual —
    /// see `mir::lower`).
    StackAlloc {
        dest: LocalId,
        bytes: i64,
    },
    /// Allocates `bytes` (runtime value) from the bump heap and stores the base
    /// address in `dest`. Used when a size is only known at run time (e.g.
    /// growing a coroutine spill buffer).
    HeapAlloc {
        dest: LocalId,
        bytes: LocalId,
    },
    /// Address of a MIR function ([`MirType::FuncRef`] result).
    FuncAddr {
        dest: LocalId,
        name: String,
    },
    /// Indirect call through a [`MirType::FuncRef`] with an explicit signature.
    CallIndirect {
        dest: Option<LocalId>,
        callee: LocalId,
        args: Vec<LocalId>,
        sig: CallSig,
    },
    /// Comparisons always produce a `bool`.
    Compare {
        dest: LocalId,
        op: CmpOp,
        left: LocalId,
        right: LocalId,
    },
    Jump {
        target: BlockId,
    },
    /// Goto to a label outside the current activation (§5.4.18): abandon nested
    /// calls and resume in the enclosing function that defines `label`.
    GotoEscape {
        label: String,
    },
    Branch {
        cond: LocalId,
        then_block: BlockId,
        else_block: BlockId,
    },
    CallOutText {
        string_id: usize,
    },
    /// Prints the runtime text frame referenced by `src`.
    CallOutTextLocal {
        src: LocalId,
    },
    CallOutImage,
    Call {
        dest: Option<LocalId>,
        name: String,
        args: Vec<LocalId>,
    },
    /// Abort execution with a diagnostic message (e.g. switch subscript OOB).
    Abort {
        message: String,
    },
    Return {
        value: Option<LocalId>,
    },
    /// Allocates a fresh N-D integer array from `bounds` — flattened
    /// `(low, high)` pairs, one per dimension — and stores the resulting
    /// descriptor pointer in `dest` (a [`MirType::ArrayI64`] local). Bound
    /// expressions are `i64` locals so non-literal declarations (e.g.
    /// `a(1:n, 1:m)`) lower like any other expression. Any dimension with
    /// `low > high` yields a legal (empty) array; every access then fails the
    /// bounds check at runtime, matching the interpreter.
    AllocArray {
        dest: LocalId,
        bounds: Vec<(LocalId, LocalId)>,
    },
    /// Reads `array[indices…]` into `dest`. Out-of-bounds subscripts abort at
    /// runtime (see `simrt_array_load_i64`).
    ArrayLoad {
        dest: LocalId,
        array: LocalId,
        indices: Vec<LocalId>,
    },
    /// Writes `value` into `array[indices…]`. Out-of-bounds subscripts abort
    /// at runtime (see `simrt_array_store_i64`).
    ArrayStore {
        array: LocalId,
        indices: Vec<LocalId>,
        value: LocalId,
    },
    /// Initializes `dest` to notext (a declared `text` variable with no
    /// initializer).
    TextNotext {
        dest: LocalId,
    },
    /// Builds a constant literal text frame in `dest` from `strings[string_id]`.
    TextFromLiteral {
        dest: LocalId,
        string_id: usize,
    },
    /// Duplicates `src`'s character content into a fresh mutable frame in
    /// `dest`.
    TextCopy {
        dest: LocalId,
        src: LocalId,
    },
    /// Deep-copies an integer or text array descriptor for call-by-value
    /// array transmission (§4.6.2).
    ArrayCopy {
        dest: LocalId,
        src: LocalId,
    },
    /// Builds a fresh mutable frame of `n` spaces in `dest` (`blanks(n)`).
    /// `n == 0` yields notext; `n < 0` aborts at runtime.
    TextBlanks {
        dest: LocalId,
        n: LocalId,
    },
    /// Concatenates `left` and `right` into a fresh mutable frame in `dest`.
    TextConcat {
        dest: LocalId,
        left: LocalId,
        right: LocalId,
    },
    /// Simula `:=` text assignment into `dest` from `src` (padded copy when
    /// `dest` already references a fixed-length frame).
    TextAssign {
        dest: LocalId,
        src: LocalId,
    },
    /// Simula `:-` reference assignment: share `src`'s object/start/length
    /// into `dest`, preserving `dest`'s `pos`.
    TextRefAssign {
        dest: LocalId,
        src: LocalId,
    },
    /// Content equality of two text frames (`=` / `<>`), producing a bool.
    TextContentEq {
        dest: LocalId,
        left: LocalId,
        right: LocalId,
    },
    /// Lexicographic content ordering of two text frames (`-1` / `0` / `1`).
    /// Used to implement text ranking `<` / `<=` / `>` / `>=`.
    TextContentCmp {
        dest: LocalId,
        left: LocalId,
        right: LocalId,
    },
    /// Reference equality of two text frames (`==` / `=/=`): same object,
    /// start, and length (both notext are equal).
    TextRefEq {
        dest: LocalId,
        left: LocalId,
        right: LocalId,
    },
    /// `frame.length` — character count of the text frame (`i64`).
    TextLength {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.constant` — whether the text object is constant (bool).
    TextConstant {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.start` — 1-based start index within the main frame (`i64`).
    TextStart {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.main` — text frame spanning the whole text object.
    TextMain {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.pos` — 1-based position indicator (`i64`).
    TextPos {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.more` — whether `pos <= length` (bool).
    TextMore {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.setpos(index)` — clamp `pos` like the interpreter.
    TextSetpos {
        frame: LocalId,
        index: LocalId,
    },
    /// `frame.getchar` — next codepoint as `i64`, advancing `pos`. Aborts when
    /// not `more` (matching interpreter `"pos out of range"`).
    TextGetchar {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.putchar(ch)` — write codepoint at `pos` and advance. Aborts on
    /// notext/constant/`pos` out of range.
    TextPutchar {
        frame: LocalId,
        ch: LocalId,
    },
    /// `frame.getint` — parse a leading integer item from the frame content
    /// (deedit), advancing `pos`. Aborts on `"no numeric item"` / overflow.
    TextGetint {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.putint(value)` — right-align an integer into the frame (edit).
    /// Aborts on notext/constant frames or overlong values (asterisks).
    TextPutint {
        frame: LocalId,
        value: LocalId,
    },
    /// `frame.getfrac` — parse a grouped numeric item (digits, ignoring
    /// spaces/`.`/`,`) into an integer (deedit).
    TextGetfrac {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.putfrac(value, places)` — edit a fixed-point grouped numeral.
    TextPutfrac {
        frame: LocalId,
        value: LocalId,
        places: LocalId,
    },
    /// `frame.getreal` — parse a real item (deedit) into `f64`.
    TextGetreal {
        dest: LocalId,
        frame: LocalId,
    },
    /// `frame.putfix(value, places)` — fixed-point edit of an `f64`.
    TextPutfix {
        frame: LocalId,
        value: LocalId,
        places: LocalId,
    },
    /// `frame.putreal(value, n)` — scientific-form edit of an `f64`.
    TextPutreal {
        frame: LocalId,
        value: LocalId,
        places: LocalId,
        /// Exponent field width: 2 for REAL, 3 for LONG REAL (Simula putreal).
        exp_digits: i64,
    },
    /// `frame.sub(i, n)` — subframe sharing `frame`'s object buffer (`i` and
    /// `n` are 1-based Simula indices/count). `n == 0` yields notext; invalid
    /// bounds abort at runtime (`"sub out of frame"`).
    TextSub {
        dest: LocalId,
        frame: LocalId,
        i: LocalId,
        n: LocalId,
    },
    /// `frame.strip` — trim trailing blanks into a fresh subframe (notext when
    /// the trimmed content is empty).
    TextStrip {
        dest: LocalId,
        frame: LocalId,
    },
    /// ENVIRONMENT `upcase(frame)` — ASCII-uppercase the frame in place;
    /// sets `pos` to 1. Aborts on notext/constant.
    TextUpcase {
        frame: LocalId,
    },
    /// ENVIRONMENT `lowcase(frame)` — ASCII-lowercase the frame in place;
    /// sets `pos` to 1. Aborts on notext/constant.
    TextLowcase {
        frame: LocalId,
    },
    /// `none` — a null object reference stored in `dest` ([`MirType::ObjectRef`]).
    ConstNone {
        dest: LocalId,
    },
    /// Allocates a fresh object of `size` bytes with `class_id` written into
    /// the header (see `simrt_object_alloc`) and stores the pointer in
    /// `dest`.
    NewObject {
        dest: LocalId,
        class_id: i64,
        size: i64,
    },
    /// Reads an `i64` attribute at byte `offset` from `object`. A null
    /// (`none`) object aborts at runtime.
    ///
    /// `class_qual` is a **point-in-time snapshot** (taken by the lowering
    /// helper that emits this op) of `object`'s best-known WasmGC class
    /// qualifier *at this exact program point* — deliberately independent
    /// of `Local::class_qual`, which is a single mutable slot per `LocalId`
    /// for the whole function and gets overwritten by any later `:-`
    /// reassignment of `object` to a different concrete subtype
    /// (`ref(D) Df; Outtext(Df.t); Df :- new E(...)` — simtst33). Reading
    /// `Local::class_qual` at codegen time (which runs after lowering
    /// finishes) would see the *last* qualifier ever assigned to `object`,
    /// not the one true at this instruction, and `ref.cast` to the wrong
    /// concrete struct traps under real WasmGC subtyping (harmless only by
    /// accident when every class is final and structurally-identical final
    /// structs get canonicalized to the same engine type). WasmGC codegen
    /// must prefer this field over `Local::class_qual` in
    /// `resolve_gc_field`; other backends ignore it.
    FieldLoadI64 {
        dest: LocalId,
        object: LocalId,
        offset: i64,
        class_qual: Option<String>,
    },
    /// Writes `value` into the `i64` attribute at byte `offset` of `object`.
    /// A null (`none`) object aborts at runtime. See [`Op::FieldLoadI64`]'s
    /// `class_qual` doc for why this is a point-in-time snapshot distinct
    /// from `Local::class_qual`.
    FieldStoreI64 {
        object: LocalId,
        offset: i64,
        value: LocalId,
        class_qual: Option<String>,
    },
    /// True when `object` is a null (`none`) object reference.
    ObjectIsNone {
        dest: LocalId,
        object: LocalId,
    },
    /// Reads the `class_id` header from `object`, or `-1` when `object` is
    /// `none` (for inspect / type tests without aborting).
    ObjectClassIdSafe {
        dest: LocalId,
        object: LocalId,
    },
    /// Reads one line from stdin (native `simrt_in_line`, WASI `fd_read`,
    /// or browser polyfill) into a fresh text frame stored in `dest`. A
    /// trailing `\n` (and a preceding `\r`) is stripped when present. Backed
    /// by a fixed 256-byte host-side read buffer MVP.
    CallInLine {
        dest: LocalId,
    },
    /// Formats `value` as decimal text and appends it to the current output
    /// Free `outint(i, w)` on SYSOUT (§10.5.8) — both arguments required.
    CallOutInt {
        value: LocalId,
        width: LocalId,
    },
    /// Free `outreal(r, n, w)` on SYSOUT (§10.5.8): real `value`, `n` decimal
    /// digits, `w` field width.
    CallOutReal {
        value: LocalId,
        digits: LocalId,
        width: LocalId,
    },
    /// Free `outfix(r, n, w)` on SYSOUT (§10.5.8).
    CallOutFix {
        value: LocalId,
        digits: LocalId,
        width: LocalId,
    },
    /// Free `outfrac(i, n, w)` on SYSOUT (§10.5.8): integer `value` scaled by
    /// `10**n` decimal digits, `w` field width.
    CallOutFrac {
        value: LocalId,
        digits: LocalId,
        width: LocalId,
    },
    /// Free `OutChar(c)` — write one character (i64 codepoint) at SysOut `pos`.
    CallOutChar {
        ch: LocalId,
    },
    /// Free `BreakOutImage` — flush SysOut chars `1..pos-1` + newline, reset.
    CallBreakOutImage,
    /// Free `InImage` — read one stdin line into the SysIn image.
    CallInImage,
    /// Free `InChar` — next SysIn image character into `dest` (i64 codepoint).
    CallInChar {
        dest: LocalId,
    },
    /// Free `Endfile` — SysIn end-of-file flag into `dest` (bool).
    CallEndfile {
        dest: LocalId,
    },
    /// `sysin` — singleton terminal `InFile` object pointer.
    CallSysIn {
        dest: LocalId,
    },
    /// `sysout` — singleton terminal `PrintFile` object pointer.
    CallSysOut {
        dest: LocalId,
    },
    /// Register a newly constructed InFile/OutFile/PrintFile in the runtime
    /// file table (`mode`: 0 = in, 1 = out).
    CallBasicioRegisterFile {
        object: LocalId,
        path: LocalId,
        mode: i64,
    },
    CallBasicioOpen {
        dest: LocalId,
        object: LocalId,
        /// Text image buffer (`fileimage`), not the path.
        fileimage: LocalId,
    },
    /// Parameterless bytefile `open` (§10.9.1 / §10.10.1).
    CallBasicioOpenByte {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioClose {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioIsOpen {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioOutText {
        object: LocalId,
        text: LocalId,
    },
    CallBasicioOutChar {
        object: LocalId,
        ch: LocalId,
    },
    CallBasicioOutImage {
        object: LocalId,
    },
    CallBasicioBreakOutImage {
        object: LocalId,
    },
    CallBasicioInImage {
        object: LocalId,
    },
    CallBasicioInChar {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioLastItem {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioInInt {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioInReal {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioInFrac {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioInText {
        dest: LocalId,
        object: LocalId,
        width: LocalId,
    },
    CallBasicioEndfile {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioInByte {
        dest: LocalId,
        object: LocalId,
    },
    CallBasicioOutByte {
        object: LocalId,
        value: LocalId,
    },
    /// DirectFile `locate(i)` (§10.6).
    CallBasicioLocate {
        object: LocalId,
        loc: LocalId,
    },
    /// DirectFile `location` (§10.6).
    CallBasicioLocation {
        dest: LocalId,
        object: LocalId,
    },
    /// DirectFile / DirectByteFile `lastloc`.
    CallBasicioLastloc {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `outreal(r, n, w)` bound to a file object.
    CallBasicioOutReal {
        object: LocalId,
        value: LocalId,
        digits: LocalId,
        width: LocalId,
        /// Exponent digit count (2 for REAL / DirectFile; 3 for LONG REAL /
        /// PrintFile in CBL86).
        exp_digits: i64,
    },
    /// BASICIO `outfix(r, n, w)` bound to a file object.
    CallBasicioOutFix {
        object: LocalId,
        value: LocalId,
        digits: LocalId,
        width: LocalId,
    },
    /// BASICIO `outfrac(i, n, w)` bound to a file object.
    CallBasicioOutFrac {
        object: LocalId,
        value: LocalId,
        digits: LocalId,
        width: LocalId,
    },
    /// BASICIO `outint(i, w)` bound to a file object.
    CallBasicioOutInt {
        object: LocalId,
        value: LocalId,
        width: LocalId,
    },
    /// PrintFile `line` (§10.7) — current line number on the page.
    CallBasicioLine {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `image` (§10.3) — current image content as a text value.
    CallBasicioImage {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `pos` (§10.3) — 1-based position in the current image.
    CallBasicioPos {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `length` (§10.3) — length of the current image.
    CallBasicioLength {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `image :- text` — replace the current image content.
    CallBasicioSetImage {
        object: LocalId,
        text: LocalId,
    },
    /// BASICIO `setpos(i)` (§10.3) — set the current image position.
    CallBasicioSetpos {
        object: LocalId,
        index: LocalId,
    },
    /// BASICIO `filename` (§10.1) — constructor path as a text value.
    CallBasicioFilename {
        dest: LocalId,
        object: LocalId,
    },
    /// BASICIO `setaccess(mode)` (§10.1.1) — apply access-mode text; returns bool.
    CallBasicioSetAccess {
        dest: LocalId,
        object: LocalId,
        mode: LocalId,
    },
    /// BASICIO `eject(n)` (§10.7.1) — set PrintFile line (and maybe new page).
    CallBasicioEject {
        object: LocalId,
        line: LocalId,
    },
    /// BASICIO `linesperpage(n)` (§10.7) — set page length; returns previous.
    /// When `n` is absent at the source, lowering passes the current value so
    /// the call is a pure getter.
    CallBasicioLinesPerPage {
        dest: LocalId,
        object: LocalId,
        n: LocalId,
    },
    /// BASICIO `inrecord` (§10.4.2) — read without space-fill; true if truncated.
    CallBasicioInRecord {
        dest: LocalId,
        object: LocalId,
    },
    CallTerminateProgram,
    /// ENVIRONMENT / runtime helper call (`decimalmark`, `lowten`, `sqrt`,
    /// `sin`, `cos`, `ln`, `exp`, `arctan`, `mod`, `sign`, `abs`, `draw`,
    /// `randint`, `uniform`, `normal`, `negexp`, `poisson`). Result type is
    /// that of `dest`; codegen maps `name` to `simrt_*`. Random helpers
    /// take a trailing [`MirType::RefI64`] stream pointer updated in place.
    CallEnv {
        dest: LocalId,
        name: String,
        args: Vec<LocalId>,
    },
    /// Whole-file `fileExists(path)` → boolean (native runtime; wasm rejects).
    CallFileExists {
        dest: LocalId,
        path: LocalId,
    },
    /// Whole-file `fileRead(path)` → text (native runtime; wasm rejects).
    CallFileRead {
        dest: LocalId,
        path: LocalId,
    },
    /// Whole-file `fileWrite(path, contents)` statement (native; wasm rejects).
    CallFileWrite {
        path: LocalId,
        contents: LocalId,
    },
    /// Start a Simulation-prefixed block: insert MAIN into the runtime SQS at
    /// time 0 (see `simrt_sim_begin`).
    SimBegin,
    /// Tear down Simulation SQS state (`simrt_sim_end`).
    SimEnd,
    /// `hold(dt)`: reschedule the current process at `time + max(dt, 0)` and
    /// advance `current` to the new SQS head.
    SimHold {
        dt: LocalId,
    },
    /// Direct `activate x`: schedule `process` at the current time with prior
    /// ordering (no-op if already scheduled).
    SimActivateDirect {
        process: LocalId,
    },
    /// Timed `activate x delay t` / `activate x at t` (`mode`: 0=delay, 1=at).
    SimActivateTimed {
        process: LocalId,
        t: LocalId,
        mode: i64,
        prior: bool,
        reac: bool,
    },
    /// `activate x before y` / `after y` (`before` selects the variant).
    SimActivateRelative {
        process: LocalId,
        other: LocalId,
        before: bool,
    },
    /// `passivate`: remove the current process from the SQS and advance.
    SimPassivate,
    /// Make the head of the sequencing set operative (§12): a chapter 7 transfer
    /// to that process's component, or back to MAIN when the set names it. Emitted
    /// after every operation that can reorder the set.
    SimTransferToHead,
    /// The active process reaches its final end: it leaves the sequencing set and
    /// the next process takes over. Does not return.
    SimTerminateCurrent {
        process: LocalId,
    },
    /// Remove `process` from the SQS (process termination / detach).
    SimCancel {
        process: LocalId,
    },
    /// Cancel MAIN from the SQS after the Simulation main body completes.
    SimFinishMain,
    /// `time` — current simulation time (`f64`).
    SimTime {
        dest: LocalId,
    },
    /// Whether the SQS current process is MAIN (`bool`).
    SimIsMainCurrent {
        dest: LocalId,
    },
    /// Whether SQS has a current process (`bool`) — false when the queue is empty.
    SimHasCurrent {
        dest: LocalId,
    },
    /// Current scheduled process object (may be MAIN sentinel).
    SimCurrent {
        dest: LocalId,
    },
    /// The Simulation `main` process (sentinel MAIN object).
    SimMain {
        dest: LocalId,
    },
    /// Whether `process` is idle (not scheduled on the SQS).
    SimIdle {
        dest: LocalId,
        process: LocalId,
    },
    /// Whether `process` has completed its body (§12.1 terminated).
    SimTerminated {
        dest: LocalId,
        process: LocalId,
    },
    /// Event time of a scheduled `process` (errors if idle).
    SimEvtime {
        dest: LocalId,
        process: LocalId,
    },
    /// Next process after `process` in the SQS (`nextev`, §12.1); none if idle
    /// or last.
    SimNextev {
        dest: LocalId,
        process: LocalId,
    },
    /// Register the Head class id so SIMSET suc/pred can filter Head vs Link.
    SimsetSetHeadClassId {
        class_id: i64,
    },
    /// Initialize an empty Head ring (`SUC = PRED = head`).
    SimsetInitHead {
        head: LocalId,
    },
    /// Remove `object` from its SIMSET list.
    SimsetOut {
        object: LocalId,
    },
    /// Insert `object` immediately before `ptr` in a SIMSET list.
    SimsetPrecede {
        object: LocalId,
        ptr: LocalId,
    },
    /// Insert `object` immediately after `ptr` in a SIMSET list.
    SimsetFollow {
        object: LocalId,
        ptr: LocalId,
    },
    /// `into(head)` — insert as last member of `head` (`precede(head)`).
    SimsetInto {
        object: LocalId,
        head: LocalId,
    },
    /// `suc` / `first` — next Link, or none.
    SimsetSuc {
        dest: LocalId,
        object: LocalId,
    },
    /// `pred` / `last` — previous Link, or none.
    SimsetPred {
        dest: LocalId,
        object: LocalId,
    },
    /// `empty` — whether a Head has no members.
    SimsetEmpty {
        dest: LocalId,
        head: LocalId,
    },
    /// `cardinal` — number of Link members in a Head.
    SimsetCardinal {
        dest: LocalId,
        head: LocalId,
    },

    // Quasi-parallel sequencing (chapter 7) over per-component stacks. Each op
    // is one call into `runtime/sequencing.c`; the runtime owns the state
    // machine, so lowering only has to say which object is meant.
    /// Entering a subblock or prefixed block that declares a class creates a
    /// quasi-parallel system (7.2). `block` identifies the block in the source
    /// so a generator elsewhere can name this system head.
    SeqSystemEnter {
        dest: LocalId,
        block: i64,
    },
    SeqSystemExit {
        system: LocalId,
    },
    /// Prepares an object's component on its own stack, entering at `entry`
    /// with the object as its argument. `declaring_block` is the system head
    /// declaring the class, or zero for an object that can only be an
    /// independent component (7.2).
    SeqObjectCreate {
        dest: LocalId,
        declaring_block: i64,
        entry: LocalId,
        object: LocalId,
    },
    /// Runs a freshly created body attached to the generating block instance.
    SeqObjectStart {
        component: LocalId,
    },
    /// Notes a prefixed block instance, whose detach attribute has no effect
    /// (7.3.1).
    SeqBlockInstance {
        object: LocalId,
    },
    // The rest name their object by reference, as chapter 7 does; the runtime
    // maps it to the component.
    /// 7.3.1
    SeqDetach {
        object: LocalId,
    },
    /// 7.3.2
    SeqCall {
        object: LocalId,
    },
    /// 7.3.3
    SeqResume {
        object: LocalId,
    },
    /// 7.3.4 — the PSC passing through a class object's final end.
    SeqTerminate {
        object: LocalId,
    },
    Nop,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConstI64 { dest, value } => write!(f, "{dest} = const.i64 {value}"),
            Self::ConstF64 { dest, value } => write!(f, "{dest} = const.f64 {value}"),
            Self::ConstBool { dest, value } => write!(f, "{dest} = const.bool {value}"),
            Self::I64ToF64 { dest, src } => write!(f, "{dest} = i64_to_f64 {src}"),
            Self::F64ToI64 { dest, src } => write!(f, "{dest} = f64_to_i64 {src}"),
            Self::Copy { dest, src } => write!(f, "{dest} = copy {src}"),
            Self::Binary {
                dest,
                op,
                left,
                right,
            } => write!(f, "{dest} = {op} {left}, {right}"),
            Self::Unary { dest, op, src } => write!(f, "{dest} = {op} {src}"),
            Self::LoadLocal { dest, local } => write!(f, "{dest} = load {local}"),
            Self::StoreLocal { local, src } => write!(f, "store {local}, {src}"),
            Self::LocalAddr { dest, local } => write!(f, "{dest} = addr_of {local}"),
            Self::FieldAddr {
                dest,
                object,
                offset,
            } => write!(f, "{dest} = field_addr {object}, offset={offset}"),
            Self::LoadRefI64 { dest, ptr, offset } => {
                write!(f, "{dest} = load.ref.i64 {ptr}, offset={offset}")
            }
            Self::StoreRefI64 { ptr, src, offset } => {
                write!(f, "store.ref.i64 {ptr}, {src}, offset={offset}")
            }
            Self::StackAlloc { dest, bytes } => write!(f, "{dest} = stack_alloc {bytes}"),
            Self::HeapAlloc { dest, bytes } => write!(f, "{dest} = heap_alloc {bytes}"),
            Self::FuncAddr { dest, name } => write!(f, "{dest} = func_addr {name}"),
            Self::CallIndirect {
                dest,
                callee,
                args,
                sig,
            } => {
                if let Some(dest) = dest {
                    write!(f, "{dest} = call_indirect {callee}(")?;
                } else {
                    write!(f, "call_indirect {callee}(")?;
                }
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")?;
                if let Some(result) = &sig.result {
                    write!(f, " -> {result}")?;
                }
                Ok(())
            }
            Self::Compare {
                dest,
                op,
                left,
                right,
            } => write!(f, "{dest} = cmp.{op} {left}, {right}"),
            Self::Jump { target } => write!(f, "jump {target}"),
            Self::GotoEscape { label } => write!(f, "goto_escape {label:?}"),
            Self::Branch {
                cond,
                then_block,
                else_block,
            } => {
                write!(f, "branch {cond}, {then_block}, {else_block}")
            }
            Self::CallOutText { string_id } => write!(f, "call out_text str#{string_id}"),
            Self::CallOutTextLocal { src } => write!(f, "call out_text {src}"),
            Self::CallOutImage => write!(f, "call out_image"),
            Self::Call { dest, name, args } => {
                if let Some(dest) = dest {
                    write!(f, "{dest} = call {name}(")?;
                } else {
                    write!(f, "call {name}(")?;
                }
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Self::Abort { message } => write!(f, "abort {message:?}"),
            Self::Return { value } => match value {
                Some(value) => write!(f, "return {value}"),
                None => write!(f, "return"),
            },
            Self::AllocArray { dest, bounds } => {
                let pairs: Vec<_> = bounds
                    .iter()
                    .map(|(low, high)| format!("{low}:{high}"))
                    .collect();
                write!(f, "{dest} = alloc_array {}", pairs.join(", "))
            }
            Self::ArrayLoad {
                dest,
                array,
                indices,
            } => {
                let idx = indices
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{dest} = array_load {array}, [{idx}]")
            }
            Self::ArrayStore {
                array,
                indices,
                value,
            } => {
                let idx = indices
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "array_store {array}, [{idx}], {value}")
            }
            Self::TextNotext { dest } => write!(f, "{dest} = text.notext"),
            Self::TextFromLiteral { dest, string_id } => {
                write!(f, "{dest} = text.literal str#{string_id}")
            }
            Self::TextCopy { dest, src } => write!(f, "{dest} = text.copy {src}"),
            Self::ArrayCopy { dest, src } => write!(f, "{dest} = array.copy {src}"),
            Self::TextBlanks { dest, n } => write!(f, "{dest} = text.blanks {n}"),
            Self::TextConcat { dest, left, right } => {
                write!(f, "{dest} = text.concat {left}, {right}")
            }
            Self::TextAssign { dest, src } => write!(f, "text.assign {dest}, {src}"),
            Self::TextRefAssign { dest, src } => write!(f, "text.ref_assign {dest}, {src}"),
            Self::TextContentEq { dest, left, right } => {
                write!(f, "{dest} = text.content_eq {left}, {right}")
            }
            Self::TextContentCmp { dest, left, right } => {
                write!(f, "{dest} = text.content_cmp {left}, {right}")
            }
            Self::TextRefEq { dest, left, right } => {
                write!(f, "{dest} = text.ref_eq {left}, {right}")
            }
            Self::TextLength { dest, frame } => write!(f, "{dest} = text.length {frame}"),
            Self::TextConstant { dest, frame } => write!(f, "{dest} = text.constant {frame}"),
            Self::TextStart { dest, frame } => write!(f, "{dest} = text.start {frame}"),
            Self::TextMain { dest, frame } => write!(f, "{dest} = text.main {frame}"),
            Self::TextPos { dest, frame } => write!(f, "{dest} = text.pos {frame}"),
            Self::TextMore { dest, frame } => write!(f, "{dest} = text.more {frame}"),
            Self::TextSetpos { frame, index } => write!(f, "text.setpos {frame}, {index}"),
            Self::TextGetchar { dest, frame } => write!(f, "{dest} = text.getchar {frame}"),
            Self::TextPutchar { frame, ch } => write!(f, "text.putchar {frame}, {ch}"),
            Self::TextGetint { dest, frame } => write!(f, "{dest} = text.getint {frame}"),
            Self::TextPutint { frame, value } => write!(f, "text.putint {frame}, {value}"),
            Self::TextGetfrac { dest, frame } => write!(f, "{dest} = text.getfrac {frame}"),
            Self::TextPutfrac {
                frame,
                value,
                places,
            } => write!(f, "text.putfrac {frame}, {value}, {places}"),
            Self::TextGetreal { dest, frame } => write!(f, "{dest} = text.getreal {frame}"),
            Self::TextPutfix {
                frame,
                value,
                places,
            } => write!(f, "text.putfix {frame}, {value}, {places}"),
            Self::TextPutreal {
                frame,
                value,
                places,
                exp_digits,
            } => write!(
                f,
                "text.putreal {frame}, {value}, {places}, exp_digits={exp_digits}"
            ),
            Self::TextSub { dest, frame, i, n } => write!(f, "{dest} = text.sub {frame}, {i}, {n}"),
            Self::TextStrip { dest, frame } => write!(f, "{dest} = text.strip {frame}"),
            Self::TextUpcase { frame } => write!(f, "text.upcase {frame}"),
            Self::TextLowcase { frame } => write!(f, "text.lowcase {frame}"),
            Self::ConstNone { dest } => write!(f, "{dest} = const.none"),
            Self::NewObject {
                dest,
                class_id,
                size,
            } => write!(f, "{dest} = new_object class_id={class_id} size={size}"),
            Self::FieldLoadI64 {
                dest,
                object,
                offset,
                ..
            } => write!(f, "{dest} = field_load.i64 {object}, offset={offset}"),
            Self::FieldStoreI64 {
                object,
                offset,
                value,
                ..
            } => write!(f, "field_store.i64 {object}, offset={offset}, {value}"),
            Self::ObjectIsNone { dest, object } => write!(f, "{dest} = object.is_none {object}"),
            Self::ObjectClassIdSafe { dest, object } => {
                write!(f, "{dest} = object.class_id_safe {object}")
            }
            Self::CallInLine { dest } => write!(f, "{dest} = call in_line"),
            Self::CallOutInt { value, width } => {
                write!(f, "call out_int {value}, {width}")
            }
            Self::CallOutReal {
                value,
                digits,
                width,
            } => {
                write!(f, "call out_real {value}, {digits}, {width}")
            }
            Self::CallOutFix {
                value,
                digits,
                width,
            } => {
                write!(f, "call out_fix {value}, {digits}, {width}")
            }
            Self::CallOutFrac {
                value,
                digits,
                width,
            } => {
                write!(f, "call out_frac {value}, {digits}, {width}")
            }
            Self::CallOutChar { ch } => write!(f, "call out_char {ch}"),
            Self::CallBreakOutImage => write!(f, "call break_out_image"),
            Self::CallInImage => write!(f, "call in_image"),
            Self::CallInChar { dest } => write!(f, "{dest} = call in_char"),
            Self::CallEndfile { dest } => write!(f, "{dest} = call endfile"),
            Self::CallSysIn { dest } => write!(f, "{dest} = call sysin"),
            Self::CallSysOut { dest } => write!(f, "{dest} = call sysout"),
            Self::CallBasicioRegisterFile { object, path, mode } => write!(
                f,
                "call basicio.register_file {object}, {path}, mode={mode}"
            ),
            Self::CallBasicioOpen {
                dest,
                object,
                fileimage,
            } => write!(f, "call basicio.open {dest} = {object}, {fileimage}"),
            Self::CallBasicioOpenByte { dest, object } => {
                write!(f, "call basicio.open_byte {dest} = {object}")
            }
            Self::CallBasicioClose { dest, object } => {
                write!(f, "call basicio.close {dest} = {object}")
            }
            Self::CallBasicioIsOpen { dest, object } => {
                write!(f, "{dest} = call basicio.isopen {object}")
            }
            Self::CallBasicioOutText { object, text } => {
                write!(f, "call basicio.outtext {object}, {text}")
            }
            Self::CallBasicioOutChar { object, ch } => {
                write!(f, "call basicio.outchar {object}, {ch}")
            }
            Self::CallBasicioOutImage { object } => {
                write!(f, "call basicio.outimage {object}")
            }
            Self::CallBasicioBreakOutImage { object } => {
                write!(f, "call basicio.breakoutimage {object}")
            }
            Self::CallBasicioInImage { object } => {
                write!(f, "call basicio.inimage {object}")
            }
            Self::CallBasicioInChar { dest, object } => {
                write!(f, "{dest} = call basicio.inchar {object}")
            }
            Self::CallBasicioLastItem { dest, object } => {
                write!(f, "{dest} = call basicio.lastitem {object}")
            }
            Self::CallBasicioInInt { dest, object } => {
                write!(f, "{dest} = call basicio.inint {object}")
            }
            Self::CallBasicioInReal { dest, object } => {
                write!(f, "{dest} = call basicio.inreal {object}")
            }
            Self::CallBasicioInFrac { dest, object } => {
                write!(f, "{dest} = call basicio.infrac {object}")
            }
            Self::CallBasicioInText {
                dest,
                object,
                width,
            } => write!(f, "{dest} = call basicio.intext {object}, {width}"),
            Self::CallBasicioEndfile { dest, object } => {
                write!(f, "{dest} = call basicio.endfile {object}")
            }
            Self::CallBasicioInByte { dest, object } => {
                write!(f, "{dest} = call basicio.inbyte {object}")
            }
            Self::CallBasicioOutByte { object, value } => {
                write!(f, "call basicio.outbyte {object}, {value}")
            }
            Self::CallBasicioLocate { object, loc } => {
                write!(f, "call basicio.locate {object}, {loc}")
            }
            Self::CallBasicioLocation { dest, object } => {
                write!(f, "{dest} = call basicio.location {object}")
            }
            Self::CallBasicioLastloc { dest, object } => {
                write!(f, "{dest} = call basicio.lastloc {object}")
            }
            Self::CallBasicioOutReal {
                object,
                value,
                digits,
                width,
                exp_digits,
            } => {
                write!(
                    f,
                    "call basicio.outreal {object}, {value}, {digits}, {width}, exp={exp_digits}"
                )
            }
            Self::CallBasicioOutFix {
                object,
                value,
                digits,
                width,
            } => {
                write!(
                    f,
                    "call basicio.outfix {object}, {value}, {digits}, {width}"
                )
            }
            Self::CallBasicioOutFrac {
                object,
                value,
                digits,
                width,
            } => {
                write!(
                    f,
                    "call basicio.outfrac {object}, {value}, {digits}, {width}"
                )
            }
            Self::CallBasicioOutInt {
                object,
                value,
                width,
            } => write!(f, "call basicio.outint {object}, {value}, {width}"),
            Self::CallBasicioLine { dest, object } => {
                write!(f, "{dest} = call basicio.line {object}")
            }
            Self::CallBasicioImage { dest, object } => {
                write!(f, "{dest} = call basicio.image {object}")
            }
            Self::CallBasicioPos { dest, object } => {
                write!(f, "{dest} = call basicio.pos {object}")
            }
            Self::CallBasicioLength { dest, object } => {
                write!(f, "{dest} = call basicio.length {object}")
            }
            Self::CallBasicioSetImage { object, text } => {
                write!(f, "call basicio.set_image {object}, {text}")
            }
            Self::CallBasicioSetpos { object, index } => {
                write!(f, "call basicio.setpos {object}, {index}")
            }
            Self::CallBasicioFilename { dest, object } => {
                write!(f, "call basicio.filename {dest} = {object}")
            }
            Self::CallBasicioSetAccess { dest, object, mode } => {
                write!(f, "{dest} = call basicio.setaccess {object}, {mode}")
            }
            Self::CallBasicioEject { object, line } => {
                write!(f, "call basicio.eject {object}, {line}")
            }
            Self::CallBasicioLinesPerPage { dest, object, n } => {
                write!(f, "{dest} = call basicio.linesperpage {object}, {n}")
            }
            Self::CallBasicioInRecord { dest, object } => {
                write!(f, "{dest} = call basicio.inrecord {object}")
            }
            Self::CallTerminateProgram => write!(f, "call terminate_program"),
            Self::CallEnv { dest, name, args } => {
                write!(f, "{dest} = call_env {name}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Self::CallFileExists { dest, path } => {
                write!(f, "{dest} = call file_exists {path}")
            }
            Self::CallFileRead { dest, path } => write!(f, "{dest} = call file_read {path}"),
            Self::CallFileWrite { path, contents } => {
                write!(f, "call file_write {path}, {contents}")
            }
            Self::SimBegin => write!(f, "sim.begin"),
            Self::SimEnd => write!(f, "sim.end"),
            Self::SimHold { dt } => write!(f, "sim.hold {dt}"),
            Self::SimActivateDirect { process } => write!(f, "sim.activate_direct {process}"),
            Self::SimActivateTimed {
                process,
                t,
                mode,
                prior,
                reac,
            } => write!(
                f,
                "sim.activate_timed {process}, {t}, mode={mode}, prior={prior}, reac={reac}"
            ),
            Self::SimActivateRelative {
                process,
                other,
                before,
            } => write!(
                f,
                "sim.activate_{} {process}, {other}",
                if *before { "before" } else { "after" }
            ),
            Self::SimPassivate => write!(f, "sim.passivate"),
            Self::SimTransferToHead => write!(f, "sim.transfer_to_head"),
            Self::SimTerminateCurrent { process } => {
                write!(f, "sim.terminate_current {process}")
            }
            Self::SimCancel { process } => write!(f, "sim.cancel {process}"),
            Self::SimFinishMain => write!(f, "sim.finish_main"),
            Self::SimTime { dest } => write!(f, "{dest} = sim.time"),
            Self::SimIsMainCurrent { dest } => write!(f, "{dest} = sim.is_main_current"),
            Self::SimHasCurrent { dest } => write!(f, "{dest} = sim.has_current"),
            Self::SimCurrent { dest } => write!(f, "{dest} = sim.current"),
            Self::SimMain { dest } => write!(f, "{dest} = sim.main"),
            Self::SimIdle { dest, process } => write!(f, "{dest} = sim.idle {process}"),
            Self::SimTerminated { dest, process } => {
                write!(f, "{dest} = sim.terminated {process}")
            }
            Self::SimEvtime { dest, process } => write!(f, "{dest} = sim.evtime {process}"),
            Self::SimNextev { dest, process } => write!(f, "{dest} = sim.nextev {process}"),
            Self::SimsetSetHeadClassId { class_id } => {
                write!(f, "simset.set_head_class_id {class_id}")
            }
            Self::SimsetInitHead { head } => write!(f, "simset.init_head {head}"),
            Self::SimsetOut { object } => write!(f, "simset.out {object}"),
            Self::SimsetPrecede { object, ptr } => {
                write!(f, "simset.precede {object}, {ptr}")
            }
            Self::SimsetFollow { object, ptr } => {
                write!(f, "simset.follow {object}, {ptr}")
            }
            Self::SimsetInto { object, head } => write!(f, "simset.into {object}, {head}"),
            Self::SimsetSuc { dest, object } => write!(f, "{dest} = simset.suc {object}"),
            Self::SimsetPred { dest, object } => write!(f, "{dest} = simset.pred {object}"),
            Self::SimsetEmpty { dest, head } => write!(f, "{dest} = simset.empty {head}"),
            Self::SimsetCardinal { dest, head } => {
                write!(f, "{dest} = simset.cardinal {head}")
            }
            Self::SeqSystemEnter { dest, block } => {
                write!(f, "{dest} = seq.system_enter {block}")
            }
            Self::SeqSystemExit { system } => write!(f, "seq.system_exit {system}"),
            Self::SeqObjectCreate {
                dest,
                declaring_block,
                entry,
                object,
            } => write!(
                f,
                "{dest} = seq.object_create {declaring_block}, {entry}, {object}"
            ),
            Self::SeqObjectStart { component } => write!(f, "seq.object_start {component}"),
            Self::SeqBlockInstance { object } => write!(f, "seq.block_instance {object}"),
            Self::SeqDetach { object } => write!(f, "seq.detach {object}"),
            Self::SeqCall { object } => write!(f, "seq.call {object}"),
            Self::SeqResume { object } => write!(f, "seq.resume {object}"),
            Self::SeqTerminate { object } => write!(f, "seq.terminate {object}"),
            Self::Nop => write!(f, "nop"),
        }
    }
}
