//! Builds [`Function`]s programmatically, for the parts of the runtime the
//! compiler synthesizes rather than lowers from source.
//!
//! The lowerer's own builder is tied to the AST it walks; this one is a plain
//! block-and-local scratchpad. Everything it emits carries an empty span, since
//! no source token corresponds to it.

use super::{BasicBlock, BinOp, BlockId, CmpOp, Function, Local, LocalId, MirType, Op, SpannedOp};

/// Pointers (objects, spill buffers, funcref slots) are ordinary `i64` values
/// in MIR; backends narrow them where the target's addresses are smaller.
pub const PTR: MirType = MirType::I64;

pub struct FunctionBuilder {
    name: String,
    params: Vec<Local>,
    locals: Vec<Local>,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    result: Option<MirType>,
}

impl FunctionBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_result(name, None)
    }

    pub fn returning(name: impl Into<String>, result: MirType) -> Self {
        Self::with_result(name, Some(result))
    }

    fn with_result(name: impl Into<String>, result: Option<MirType>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            locals: Vec::new(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                ops: Vec::new(),
            }],
            current: BlockId(0),
            result,
        }
    }

    /// [`Function::local`] indexes the parameters first, so every parameter has
    /// to be declared before the first local.
    pub fn param(&mut self, name: &str, ty: MirType) -> LocalId {
        assert!(
            self.locals.is_empty(),
            "{}: parameters must be declared before locals",
            self.name
        );
        self.params.push(Local {
            name: name.to_string(),
            ty,
            class_qual: None,
            debug_scope: None,
        });
        LocalId(self.params.len() - 1)
    }

    pub fn local(&mut self, name: &str, ty: MirType) -> LocalId {
        self.locals.push(Local {
            name: name.to_string(),
            ty,
            class_qual: None,
            debug_scope: None,
        });
        LocalId(self.params.len() + self.locals.len() - 1)
    }

    pub fn block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len());
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            ops: Vec::new(),
        });
        id
    }

    pub fn at(&mut self, block: BlockId) {
        self.current = block;
    }

    pub fn push(&mut self, op: Op) {
        self.blocks[self.current.0]
            .ops
            .push(SpannedOp { op, span: 0..0 });
    }

    pub fn konst(&mut self, value: i64) -> LocalId {
        let dest = self.local("k", MirType::I64);
        self.push(Op::ConstI64 { dest, value });
        dest
    }

    /// `none` as an [`MirType::ObjectRef`] local.
    pub fn none_object(&mut self) -> LocalId {
        let dest = self.local("none", MirType::ObjectRef);
        self.push(Op::ConstNone { dest });
        dest
    }

    /// `object == none` → bool local.
    pub fn is_none_object(&mut self, object: LocalId) -> LocalId {
        let dest = self.local("is_none", MirType::Bool);
        self.push(Op::ObjectIsNone { dest, object });
        dest
    }

    /// `*(base + offset)`.
    pub fn load(&mut self, base: LocalId, offset: i64) -> LocalId {
        let dest = self.local("v", PTR);
        self.push(Op::FieldLoadI64 {
            dest,
            object: base,
            offset,
            class_qual: None,
        });
        dest
    }

    /// Load an object reference stored at `*(base + offset)`.
    pub fn load_object(&mut self, base: LocalId, offset: i64) -> LocalId {
        let dest = self.local("oref", MirType::ObjectRef);
        self.push(Op::FieldLoadI64 {
            dest,
            object: base,
            offset,
            class_qual: None,
        });
        dest
    }

    /// `*(base + offset) = value`.
    pub fn store(&mut self, base: LocalId, offset: i64, value: LocalId) {
        self.push(Op::FieldStoreI64 {
            object: base,
            offset,
            value,
            class_qual: None,
        });
    }

    pub fn store_const(&mut self, base: LocalId, offset: i64, value: i64) {
        let value = self.konst(value);
        self.store(base, offset, value);
    }

    pub fn binary(&mut self, op: BinOp, left: LocalId, right: LocalId) -> LocalId {
        let dest = self.local("t", MirType::I64);
        self.push(Op::Binary {
            dest,
            op,
            left,
            right,
        });
        dest
    }

    pub fn add_const(&mut self, left: LocalId, value: i64) -> LocalId {
        let right = self.konst(value);
        self.binary(BinOp::Add, left, right)
    }

    pub fn mul_const(&mut self, left: LocalId, value: i64) -> LocalId {
        let right = self.konst(value);
        self.binary(BinOp::Mul, left, right)
    }

    /// Assigns to an already-declared slot. MIR is not SSA, so a loop variable
    /// is one local written on each pass rather than a fresh value.
    pub fn assign(&mut self, dest: LocalId, src: LocalId) {
        self.push(Op::Copy { dest, src });
    }

    pub fn compare(&mut self, op: CmpOp, left: LocalId, right: LocalId) -> LocalId {
        let dest = self.local("c", MirType::Bool);
        self.push(Op::Compare {
            dest,
            op,
            left,
            right,
        });
        dest
    }

    pub fn compare_const(&mut self, op: CmpOp, left: LocalId, value: i64) -> LocalId {
        let right = self.konst(value);
        self.compare(op, left, right)
    }

    pub fn branch(&mut self, cond: LocalId, then_block: BlockId, else_block: BlockId) {
        self.push(Op::Branch {
            cond,
            then_block,
            else_block,
        });
    }

    /// Branches on `left == value`, continuing in `else_block`.
    pub fn branch_if_eq(&mut self, left: LocalId, value: i64, then_block: BlockId) -> BlockId {
        let cond = self.compare_const(CmpOp::Eq, left, value);
        let otherwise = self.block();
        self.branch(cond, then_block, otherwise);
        self.at(otherwise);
        otherwise
    }

    pub fn jump(&mut self, target: BlockId) {
        self.push(Op::Jump { target });
    }

    pub fn ret(&mut self) {
        self.push(Op::Return { value: None });
    }

    pub fn ret_value(&mut self, value: LocalId) {
        self.push(Op::Return { value: Some(value) });
    }

    pub fn call(&mut self, name: &str, args: &[LocalId]) {
        self.push(Op::Call {
            dest: None,
            name: name.to_string(),
            args: args.to_vec(),
        });
    }

    pub fn call_value(&mut self, name: &str, args: &[LocalId], ty: MirType) -> LocalId {
        let dest = self.local("r", ty);
        self.push(Op::Call {
            dest: Some(dest),
            name: name.to_string(),
            args: args.to_vec(),
        });
        dest
    }

    pub fn abort(&mut self, message: &str) {
        self.push(Op::Abort {
            message: message.to_string(),
        });
    }

    /// Allocates a zero-filled record of `bytes`, including the object header
    /// that [`Op::NewObject`] writes at offset 0.
    pub fn alloc(&mut self, bytes: i64) -> LocalId {
        let dest = self.local("rec", PTR);
        self.push(Op::NewObject {
            dest,
            class_id: 0,
            size: bytes,
        });
        dest
    }

    /// Allocates `bytes` (a runtime size) from the bump heap with no class header.
    pub fn alloc_bytes(&mut self, bytes: LocalId) -> LocalId {
        let dest = self.local("buf", PTR);
        self.push(Op::HeapAlloc { dest, bytes });
        dest
    }

    pub fn func_addr(&mut self, name: &str) -> LocalId {
        let dest = self.local("fn", MirType::FuncRef);
        self.push(Op::FuncAddr {
            dest,
            name: name.to_string(),
        });
        dest
    }

    pub fn finish(self) -> Function {
        Function {
            name: self.name,
            params: self.params,
            locals: self.locals,
            entry: BlockId(0),
            blocks: self.blocks,
            labels: std::collections::HashMap::new(),
            result: self.result,
            array_elem_kinds: std::collections::HashMap::new(),
            foreign: None,
            export: None,
            debug_scopes: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_and_locals_share_one_index_space() {
        let mut builder = FunctionBuilder::returning("f", MirType::I64);
        let a = builder.param("a", MirType::I64);
        let b = builder.local("b", MirType::I64);
        let function = builder.finish();
        assert_eq!(a, LocalId(0));
        assert_eq!(b, LocalId(1));
        assert_eq!(function.local(a).name, "a");
        assert_eq!(function.local(b).name, "b");
    }

    #[test]
    fn branch_if_eq_continues_in_the_fallthrough_block() {
        let mut builder = FunctionBuilder::new("f");
        let taken = builder.block();
        let value = builder.konst(3);
        let fallthrough = builder.branch_if_eq(value, 3, taken);
        builder.ret();
        builder.at(taken);
        builder.ret();
        let function = builder.finish();
        assert!(
            function
                .block(fallthrough)
                .ops
                .iter()
                .any(|spanned| { matches!(spanned.op, Op::Return { value: None }) })
        );
    }
}
