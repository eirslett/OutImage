//! Chapter 2 type system (Simula Standard), integration tests.

mod common;

use outimage::ast::{ArithmeticLiteralKind, Expr, ExprKind, Program, StatementKind, Type};
use outimage::source::SourceFile;

fn parse_source(source: &str) -> Program {
    outimage::parse::parse(&outimage::lex::tokenize(&SourceFile::anonymous(source)).unwrap())
        .unwrap()
}

fn parse_fixture(name: &str) -> Program {
    parse_source(&common::fixture(&format!("types/{name}")))
}

fn analyze_source(source: &str) -> Result<(), outimage::CompileError> {
    let file = SourceFile::anonymous(source);
    let tokens = outimage::lex::tokenize(&file)?;
    let program = outimage::parse::parse(&tokens)?;
    outimage::semantic::analyze(&program)
}

#[test]
fn parses_all_basic_types() {
    let program = parse_fixture("all_types.sim");
    let decls = &program.blocks[0].declarations;

    assert_eq!(decls.len(), 8);
    assert_eq!(decls[0].ty, Type::Integer { short: false });
    assert_eq!(decls[1].ty, Type::Integer { short: true });
    assert_eq!(decls[2].ty, Type::Real { long: false });
    assert_eq!(decls[3].ty, Type::Real { long: true });
    assert_eq!(decls[4].ty, Type::Boolean);
    assert_eq!(decls[5].ty, Type::Character);
    assert_eq!(decls[6].ty, Type::Text);
    assert_eq!(decls[7].ty, Type::ObjectRef("Node".into()));
}

#[test]
fn parses_typed_initializers() {
    let program = parse_fixture("typed_initializers.sim");
    let decls = &program.blocks[0].declarations;

    assert_eq!(
        decls[0].items[0].initializer,
        Some(Expr::dummy(ExprKind::NumberLiteral {
            lexeme: "42".into(),
            kind: ArithmeticLiteralKind::Integer,
        }))
    );
    assert_eq!(
        decls[4].items[0].initializer,
        Some(Expr::dummy(ExprKind::BooleanLiteral(true)))
    );
    assert_eq!(
        decls[5].items[0].initializer,
        Some(Expr::dummy(ExprKind::CharacterLiteral('A')))
    );
    assert_eq!(
        decls[6].items[0].initializer,
        Some(Expr::dummy(ExprKind::StringLiteral("hello".into())))
    );
    assert_eq!(decls[7].ty, Type::ObjectRef("File".into()));
}

#[test]
fn parses_text_notext_initializer() {
    let program = parse_fixture("text_notext.sim");
    assert_eq!(
        program.blocks[0].declarations[0].items[0].initializer,
        Some(Expr::dummy(ExprKind::Notext))
    );
}

#[test]
fn parses_comma_separated_names() {
    let program = parse_fixture("comma_separated.sim");
    let items = &program.blocks[0].declarations[0].items;
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].name, "i");
    assert_eq!(items[1].name, "j");
    assert_eq!(items[2].name, "k");
}

#[test]
fn typed_programs_compile() {
    for name in [
        "all_types.sim",
        "typed_initializers.sim",
        "text_notext.sim",
        "comma_separated.sim",
        "arithmetic_compatibility.sim",
        "ref_subordination.sim",
    ] {
        let source = common::fixture(&format!("types/{name}"));
        outimage::compile_str(&source).expect("type fixture should compile");
    }
}

#[test]
fn semantic_accepts_arithmetic_compatibility() {
    analyze_source(&common::fixture("types/arithmetic_compatibility.sim")).unwrap();
}

#[test]
fn semantic_rejects_integer_initialized_with_boolean() {
    let error = analyze_source("begin integer i := true; end;").unwrap_err();
    assert!(
        error.to_string().contains("boolean") || error.to_string().contains("integer"),
        "unexpected error: {error}"
    );
}

#[test]
fn semantic_rejects_character_assigned_to_text() {
    let error = analyze_source("begin text t; character c; t := c; end;").unwrap_err();
    assert!(
        error.to_string().contains("assignment needs"),
        "unexpected error: {error}"
    );
}

#[test]
fn semantic_rejects_mismatched_ref_qualification() {
    let error = analyze_source("begin ref(A) a; ref(B) b; a :- b; end;").unwrap_err();
    assert!(
        error.to_string().contains("assignment needs") && error.to_string().contains("ref"),
        "unexpected error: {error}"
    );
}

#[test]
fn semantic_accepts_subordinate_ref_assignment() {
    analyze_source(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); real r; begin end;
         ref(Point) p; ref(Polar) q;
         p :- q; end;",
    )
    .unwrap();
}

#[test]
fn semantic_accepts_superclass_ref_to_subclass() {
    // §3.6 reference assignment is legal in both directions along a prefix
    // chain; the narrowing direction is a downcast checked at runtime rather
    // than a compile-time error.
    analyze_source(
        "begin class Point(x); integer x; begin end;
         Point class Polar(r); real r; begin end;
         ref(Polar) q; ref(Point) p;
         q :- p; end;",
    )
    .unwrap();
}

#[test]
fn semantic_rejects_ref_assignment_between_unrelated_classes() {
    let error = analyze_source(
        "begin class Point(x); integer x; begin end;
         class Colour(c); integer c; begin end;
         ref(Point) p; ref(Colour) k;
         p :- k; end;",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("assignment needs") && error.to_string().contains("ref"),
        "unexpected error: {error}"
    );
}

#[test]
fn semantic_accepts_matching_ref_qualification() {
    analyze_source("begin ref(Node) a; ref(Node) b; a :- b; end;").unwrap();
}

#[test]
fn lexer_tokenizes_short_integer_and_long_real() {
    use outimage::lex::{Keyword, TokenKind};

    let tokens: Vec<_> = outimage::lex::tokenize(&SourceFile::anonymous(
        "begin short integer si; long real lr; end;",
    ))
    .unwrap()
    .tokens
    .into_iter()
    .map(|t| t.kind)
    .collect();

    assert!(tokens.contains(&TokenKind::Keyword(Keyword::Short)));
    assert!(tokens.contains(&TokenKind::Keyword(Keyword::Integer)));
    assert!(tokens.contains(&TokenKind::Keyword(Keyword::Long)));
    assert!(tokens.contains(&TokenKind::Keyword(Keyword::Real)));
}

#[test]
fn declarations_appear_before_statements_in_block() {
    let program = parse_source(r#"begin integer i; OutText("ok"); end;"#);
    assert_eq!(program.blocks[0].declarations.len(), 1);
    assert!(matches!(
        program.blocks[0].statements[0].kind,
        StatementKind::ProcedureCall(_)
    ));
}

#[test]
fn parses_nested_block_with_declarations() {
    let program = parse_source("begin integer i; begin real r; end; end;");
    assert_eq!(program.blocks[0].declarations.len(), 1);
    let StatementKind::Compound(inner) = &program.blocks[0].statements[0].kind else {
        panic!("expected nested begin as compound statement");
    };
    assert_eq!(inner.declarations.len(), 1);
    assert_eq!(inner.declarations[0].ty, Type::Real { long: false });
}

#[test]
fn rejects_invalid_ref_syntax() {
    let file = SourceFile::anonymous("begin ref x; end;");
    let tokens = outimage::lex::tokenize(&file).unwrap();
    let error = outimage::parse::parse(&tokens).unwrap_err();
    assert!(
        error.to_string().contains("`ref`") && error.to_string().contains("(Class)"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_short_without_integer_keyword() {
    let file = SourceFile::anonymous("begin short x; end;");
    let tokens = outimage::lex::tokenize(&file).unwrap();
    let error = outimage::parse::parse(&tokens).unwrap_err();
    assert!(
        error.to_string().contains("`short`") && error.to_string().contains("integer"),
        "unexpected error: {error}"
    );
}

#[test]
fn rejects_long_without_real_keyword() {
    let file = SourceFile::anonymous("begin long x; end;");
    let tokens = outimage::lex::tokenize(&file).unwrap();
    let error = outimage::parse::parse(&tokens).unwrap_err();
    assert!(
        error.to_string().contains("`long`") && error.to_string().contains("real"),
        "unexpected error: {error}"
    );
}
