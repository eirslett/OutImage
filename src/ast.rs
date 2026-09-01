//! Abstract syntax tree types for Simula.

pub use crate::types::{ArithmeticLiteralKind, Declaration, DeclarationItem, Type};

use crate::error::Span;

/// A node paired with the source span it was parsed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }

    /// Construct a `Spanned` with a placeholder `0..0` span, for tests and
    /// call sites that have not yet been threaded through with real spans.
    pub fn dummy(node: T) -> Self {
        Self { node, span: 0..0 }
    }
}

impl<T> std::ops::Deref for Spanned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.node
    }
}

impl<T> std::ops::DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.node
    }
}

/// An array declaration (Standard §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayDeclaration {
    pub element_type: Type,
    pub segments: Vec<ArraySegment>,
    /// Source span of the full array declaration.
    pub span: crate::error::Span,
}

/// One segment in an array declaration: names sharing the same bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArraySegment {
    pub names: Vec<String>,
    pub bounds: Vec<BoundPair>,
}

/// A bound pair `lower:upper` in an array segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPair {
    pub lower: Expr,
    pub upper: Expr,
}

/// An item in an external list (§6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalItem {
    pub name: String,
    /// Optional `= "module-id"` string for linker identification.
    pub identification: Option<String>,
}

/// External procedure declaration (§6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalProcedureDeclaration {
    /// Source language kind (e.g. `Fortran`); `None` when Simula.
    pub kind: Option<String>,
    pub result_type: Option<Type>,
    pub items: Vec<ExternalItem>,
    /// `is procedure-declaration` form; body must be empty when present.
    pub specification: Option<ProcedureDeclaration>,
    /// Source span of the `external …` declaration.
    pub span: crate::error::Span,
}

/// External class declaration (§6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalClassDeclaration {
    pub items: Vec<ExternalItem>,
}

/// External declaration in an external head or block head (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalDeclaration {
    Procedure(ExternalProcedureDeclaration),
    Class(ExternalClassDeclaration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Historically held `%` directive texts. Annotation lines are now elided at
    /// lex time, so this is always empty (kept for API stability).
    pub directives: Vec<String>,
    /// Optional external head preceding the module body (§6.1).
    pub external_head: Vec<ExternalDeclaration>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Optional block-prefix procedure designator before `begin` (§4.10.1).
    pub prefix: Option<Expr>,
    pub name: String,
    /// Always empty — `%` lines are elided at lex time (see [`Program::directives`]).
    pub directives: Vec<String>,
    /// Block-level external declarations (§6.1).
    pub externals: Vec<ExternalDeclaration>,
    pub declarations: Vec<Declaration>,
    pub arrays: Vec<ArrayDeclaration>,
    pub switches: Vec<SwitchDeclaration>,
    pub procedures: Vec<ProcedureDeclaration>,
    pub classes: Vec<ClassDeclaration>,
    pub statements: Vec<Statement>,
    pub body: Vec<Block>,
}

/// A switch declaration (§5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchDeclaration {
    pub name: String,
    pub elements: Vec<DesignationalExpr>,
    /// Source span of the full switch declaration.
    pub span: crate::error::Span,
}

/// Parameter transmission mode (§4.6.2–§4.6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamMode {
    Value,
    Reference,
    Name,
}

/// A formal parameter in a procedure or class heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormalParameter {
    pub name: String,
    pub ty: Type,
    pub mode: ParamMode,
    /// Whether the transmission mode was set explicitly in a mode/value part.
    pub mode_explicit: bool,
    /// Whether this formal is specified as a procedure in the specification part.
    pub is_procedure: bool,
    /// Whether this formal is specified as a label (§4.5 / §5.4.2).
    pub is_label: bool,
    /// Whether this formal is specified as a switch (§4.5 / §5.4.2).
    pub is_switch: bool,
    /// Source span of the formal name in the heading (best-effort).
    pub span: crate::error::Span,
}

/// A procedure declaration (§5.4, body execution §4.6.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureDeclaration {
    pub result_type: Option<Type>,
    pub name: String,
    pub parameters: Vec<FormalParameter>,
    pub body: Block,
    /// Shorthand `procedure ...; external;` (stdlib compatibility).
    pub is_external: bool,
    /// Optional `= "identification"` on a defining procedure (`export:name`).
    pub identification: Option<String>,
    /// Source span of the procedure heading through its body.
    pub span: crate::error::Span,
}

/// Attribute protection metadata (Simula Standard §5.5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeProtection {
    pub protected: bool,
    pub hidden: bool,
    pub defining_class: String,
    /// Span of the `protected` specification that last set [`Self::protected`].
    pub protected_span: Option<crate::error::Span>,
    /// Span of the `hidden` specification that last set [`Self::hidden`].
    pub hidden_span: Option<crate::error::Span>,
}

/// A class declaration (§5.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDeclaration {
    /// Prefix class identifier, e.g. `point` in `point class polar`.
    pub prefix: Option<String>,
    pub name: String,
    pub parameters: Vec<FormalParameter>,
    pub specifications: Vec<Specification>,
    pub virtual_part: Vec<VirtualSpec>,
    pub protection_part: Vec<ProtectionSpec>,
    /// Resolved protection flags for attributes after concatenation.
    pub protection_map: std::collections::BTreeMap<String, AttributeProtection>,
    pub body: Block,
    /// When `true`, the class body contains an `inner` marker (split body).
    pub has_inner: bool,
    /// Optional label preceding `inner` in a split body.
    pub inner_label: Option<String>,
    /// Statements after the `inner` marker in a split body (§5.5.2.8).
    pub tail_statements: Vec<Statement>,
    /// Identifier substitutions applied to this class's main part during
    /// concatenation (§5.5.2.6–2.7). Maps original attribute spelling →
    /// renamed spelling used in the concatenated body / remote lookups.
    pub identifier_substitutions: std::collections::BTreeMap<String, String>,
    /// Source span of the class heading through its body.
    pub span: crate::error::Span,
}

/// A specification entry in a class or procedure heading (§5.4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specification {
    pub specifier: Specifier,
    pub names: Vec<String>,
}

/// Specifier kinds accepted in specification and virtual parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Specifier {
    Type(Type),
    TypeArray(Type),
    Array,
    Label,
    Switch,
    Procedure,
    TypeProcedure(Type),
}

/// A virtual quantity specification (§5.5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualSpec {
    pub specifier: Specifier,
    pub names: Vec<String>,
    /// Full procedure heading from `procedure id is procedure-declaration` (§5.5.3.6).
    /// When set, a matching attribute must use this exact heading.
    pub procedure_heading: Option<ProcedureDeclaration>,
}

/// Attribute protection specification (§5.5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectionSpec {
    pub hidden: bool,
    pub protected: bool,
    pub names: Vec<String>,
    /// Span of this `hidden` / `protected` clause, when parsed from source.
    pub span: Option<crate::error::Span>,
}

/// A Simula statement, paired with the source span it was parsed from.
///
/// `PartialEq` is implemented manually to compare only `kind`: spans are
/// source-location metadata, not semantic content, so two statements built
/// from equivalent syntax (e.g. a hand-built `Statement::dummy` in a test vs.
/// one produced by the real parser) should compare equal regardless of span.
#[derive(Debug, Clone)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

impl PartialEq for Statement {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}
impl Eq for Statement {}

impl Statement {
    pub fn new(kind: StatementKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Construct a `Statement` with a placeholder `0..0` span, for call sites
    /// that have not yet been threaded through with real spans (and tests).
    pub fn dummy(kind: StatementKind) -> Self {
        Self { kind, span: 0..0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementKind {
    ProcedureCall(ProcedureCall),
    Assignment(Assignment),
    If(IfStatement),
    While(WhileStatement),
    For(ForStatement),
    Goto(GotoStatement),
    Compound(Block),
    Labeled {
        label: String,
        statement: Box<Statement>,
    },
    /// Side-effect expression statement (e.g. remote procedure call `t.setpos(1);`).
    Expr(Expr),
    /// Empty statement (`;`) — §4.11.
    Dummy,
    /// Object generator statement — §4.7.
    ObjectGenerator(ObjectGenerator),
    /// Split-body marker (`inner`) — §5.5 split-body.
    Inner {
        label: Option<String>,
    },
    /// Connection (`inspect`) statement — §4.8.
    Inspect(InspectStatement),
    /// GPSS/Simulation `activate` statement.
    Activate(ActivateStatement),
    /// GPSS/Simulation `reactivate` statement.
    Reactivate(ReactivateStatement),
}

/// Optional timing on GPSS `activate` / `reactivate` statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimulationTiming {
    Delay(Expr),
    After(Expr),
    At(Expr),
    Before(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivateStatement {
    pub target: Expr,
    pub timing: Option<SimulationTiming>,
    pub prior: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactivateStatement {
    pub target: Expr,
    pub timing: Option<SimulationTiming>,
}
/// Object generator statement (§4.7): `new class-identifier [(params)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectGenerator {
    pub class_name: String,
    pub arguments: Vec<Expr>,
}

/// Connection statement (§4.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectStatement {
    pub object: Expr,
    pub when_clauses: Vec<WhenClause>,
    /// `inspect X do S` form (connection-block-2).
    pub do_clause: Option<Box<Statement>>,
    pub otherwise: Option<Box<Statement>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhenClause {
    pub class_name: String,
    pub body: Box<Statement>,
}

/// Conditional statement (§4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfStatement {
    pub condition: Expr,
    pub then_branch: Box<Statement>,
    pub else_branch: Option<Box<Statement>>,
}

/// While-statement (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhileStatement {
    pub condition: Expr,
    pub body: Box<Statement>,
}

/// For-statement (§4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForStatement {
    pub variable: String,
    pub elements: Vec<ForListElement>,
    pub body: Box<Statement>,
}

/// A for-list element (§4.4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForListElement {
    /// `C := V` or `C := V while B` or text-expression
    Value {
        expr: Expr,
        while_cond: Option<Expr>,
    },
    /// `C :- R` or `C :- R while B`
    Reference {
        expr: Expr,
        while_cond: Option<Expr>,
    },
    /// `A1 step A2 until A3`
    StepUntil {
        start: Expr,
        step: Expr,
        until: Expr,
    },
}

/// Goto-statement (§4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GotoStatement {
    pub target: DesignationalExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcedureCall {
    pub name: String,
    pub arguments: Vec<Expr>,
}

/// Right-hand side of an assignment (§4.1).
///
/// `value-right-part` and `reference-right-part` may be a nested assignment for
/// chained forms such as `A := B := C`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignmentRhs {
    Expr(Expr),
    Chain(Box<Assignment>),
}

impl AssignmentRhs {
    pub fn as_expr(&self) -> Option<&Expr> {
        match self {
            Self::Expr(expr) => Some(expr),
            Self::Chain(_) => None,
        }
    }
}

/// An assignment statement (§4.1): value (`:=`) or reference (`:-`) assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub lhs: Variable,
    pub operator: AssignOperator,
    pub rhs: AssignmentRhs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOperator {
    /// `:=`
    Assign,
    /// `:-`
    AssignAlt,
}

/// A variable reference (§3.1): simple, subscripted, or remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Variable {
    Simple(String),
    Subscripted {
        name: String,
        subscripts: Vec<Expr>,
    },
    /// `var qua Class` as an assignment-LHS / remote-object qualifier (§3.8.2).
    Qua {
        object: Box<Variable>,
        class_name: String,
    },
    Remote {
        object: Box<Variable>,
        attribute: String,
    },
    /// Remote procedure designator as assignment target: `obj.attr(args) := rhs`.
    RemoteCall {
        object: Box<Variable>,
        attribute: String,
        arguments: Vec<Expr>,
    },
}

/// Unary operators in expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
}

/// Binary operators in expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Pow,
    TextConcat,
    And,
    Or,
    Imp,
    Eqv,
    AndThen,
    OrElse,
}

/// Relational operators (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationOp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
    Ne,
    RefEq,
    RefNe,
    Is,
    In,
}

/// A Simula expression, paired with the source span it was parsed from.
///
/// `PartialEq` is implemented manually to compare only `kind` (see
/// [`Statement`]'s `PartialEq` for rationale): spans should not affect
/// structural equality.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl PartialEq for Expr {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}
impl Eq for Expr {}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Construct an `Expr` with a placeholder `0..0` span, for call sites
    /// that have not yet been threaded through with real spans (and tests).
    pub fn dummy(kind: ExprKind) -> Self {
        Self { kind, span: 0..0 }
    }
}

/// A Simula expression (Chapters 3.2–3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    StringLiteral(String),
    CharacterLiteral(char),
    BooleanLiteral(bool),
    Notext,
    NumberLiteral {
        lexeme: String,
        kind: ArithmeticLiteralKind,
    },
    Variable(Variable),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Relation {
        op: RelationOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Paren(Box<Expr>),
    FunctionCall {
        name: String,
        arguments: Vec<Expr>,
    },
    /// Remote attribute access when the object is a general expression.
    RemoteAccess {
        object: Box<Expr>,
        attribute: String,
    },
    /// Remote procedure call: `obj.attr(args)` (§5.5.3 virtual dispatch).
    RemoteCall {
        object: Box<Expr>,
        attribute: String,
        arguments: Vec<Expr>,
    },
    /// `none` (§3.8)
    None,
    /// `new class-identifier [(params)]` (§3.8.2)
    New {
        class_name: String,
        arguments: Option<Vec<Expr>>,
    },
    /// `this class-identifier` (§3.8.3)
    This(String),
    /// `expr qua class-identifier` (§3.8.4)
    Qua {
        object: Box<Expr>,
        class_name: String,
    },
}

/// A designational expression (§3.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignationalExpr {
    Label(String),
    SwitchDesignator {
        name: String,
        subscript: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_expr: Box<DesignationalExpr>,
        else_expr: Box<DesignationalExpr>,
    },
    Paren(Box<DesignationalExpr>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::test_support::parse_program;
    use crate::semantic;

    #[test]
    fn assignment_and_binary_expr_spans_are_nested_and_non_empty() {
        let program = parse_program("begin x := 1 + 2; end;");
        let assignment_stmt = &program.blocks[0].statements[0];
        assert!(
            !assignment_stmt.span.is_empty(),
            "assignment span should not be empty: {:?}",
            assignment_stmt.span
        );

        let StatementKind::Assignment(assignment) = &assignment_stmt.kind else {
            panic!("expected assignment statement");
        };
        let rhs = assignment.rhs.as_expr().expect("expected expression rhs");
        assert!(
            !rhs.span.is_empty(),
            "binary expr span should not be empty: {:?}",
            rhs.span
        );

        let ExprKind::Binary { left, right, .. } = &rhs.kind else {
            panic!("expected binary expression");
        };
        assert!(!left.span.is_empty());
        assert!(!right.span.is_empty());

        // The binary expression's span should fully contain both operand spans,
        // and the assignment statement's span should in turn contain the whole
        // binary expression's span.
        assert!(rhs.span.start <= left.span.start && left.span.end <= rhs.span.end);
        assert!(rhs.span.start <= right.span.start && right.span.end <= rhs.span.end);
        assert!(
            assignment_stmt.span.start <= rhs.span.start
                && rhs.span.end <= assignment_stmt.span.end
        );
    }

    #[test]
    fn semantic_error_carries_a_span() {
        let program = parse_program("begin integer x; x := true; end;");
        let error = semantic::analyze(&program).expect_err("expected semantic error");
        assert!(
            error.span.is_some(),
            "expected semantic error to carry a span, got {error:?}"
        );
    }

    #[test]
    fn empty_block_dummy_and_labeled_statements_still_parse() {
        let program = parse_program("begin end;");
        assert!(program.blocks[0].statements.is_empty());

        let program = parse_program("begin ; end;");
        assert!(matches!(
            program.blocks[0].statements[0].kind,
            StatementKind::Dummy
        ));

        let program = parse_program("begin fanfare: OutImage; end;");
        let StatementKind::Labeled { label, statement } = &program.blocks[0].statements[0].kind
        else {
            panic!("expected labeled statement");
        };
        assert_eq!(label, "fanfare");
        assert!(matches!(statement.kind, StatementKind::ProcedureCall(_)));
    }

    #[test]
    fn nested_if_and_while_statements_have_spans() {
        let program =
            parse_program("begin integer i; if i > 0 then while i > 0 do i := i - 1; end;");
        let outer = &program.blocks[0].statements[0];
        assert!(!outer.span.is_empty());

        let StatementKind::If(if_stmt) = &outer.kind else {
            panic!("expected if statement");
        };
        assert!(!if_stmt.then_branch.span.is_empty());
        assert!(
            outer.span.start <= if_stmt.then_branch.span.start
                && if_stmt.then_branch.span.end <= outer.span.end
        );

        let StatementKind::While(while_stmt) = &if_stmt.then_branch.kind else {
            panic!("expected while statement nested in then-branch");
        };
        assert!(!while_stmt.body.span.is_empty());
        assert!(
            if_stmt.then_branch.span.start <= while_stmt.body.span.start
                && while_stmt.body.span.end <= if_stmt.then_branch.span.end
        );
    }
}
