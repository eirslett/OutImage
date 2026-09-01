//! End-comment fixtures ported from tree-sitter Simula corpus (§1.8.1).
//!
//! Each case lives in `tests/fixtures/end-comment/` and is validated for
//! tokenization. Cases marked `compiles: true` must also parse and compile.

mod common;

use outimage::lex::{Keyword, TokenKind};
use outimage::source::SourceFile;

struct Case {
    name: &'static str,
    file: &'static str,
    tokens: &'static [&'static str],
    absent_idents: &'static [&'static str],
    compiles: Option<bool>,
}

fn tokenize(source: &str) -> Vec<TokenKind> {
    outimage::lex::tokenize(&SourceFile::anonymous(source))
        .expect("fixture should tokenize")
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

fn significant_tokens(kinds: &[TokenKind]) -> Vec<String> {
    kinds
        .iter()
        .filter_map(|kind| match kind {
            TokenKind::Keyword(keyword) => Some(keyword.as_str().to_string()),
            TokenKind::Identifier(name) => Some(name.clone()),
            TokenKind::Semicolon => Some(";".into()),
            _ => None,
        })
        .collect()
}

fn has_identifier(kinds: &[TokenKind], name: &str) -> bool {
    kinds.iter().any(
        |kind| matches!(kind, TokenKind::Identifier(ident) if ident.eq_ignore_ascii_case(name)),
    )
}

const CASES: &[Case] = &[
    Case {
        name: "trivial",
        file: "trivial.sim",
        tokens: &["begin", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "trivial_semicolon_after_end",
        file: "trivial_semicolon_after_end.sim",
        tokens: &["begin", "end", ";"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "trivial_semicolon_inside_block",
        file: "trivial_semicolon_inside_block.sim",
        tokens: &["begin", ";", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "trivial_semicolons_inside_and_outside",
        file: "trivial_semicolons_inside_and_outside.sim",
        tokens: &["begin", ";", "end", ";"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "trivial_procedure_call",
        file: "trivial_procedure_call.sim",
        tokens: &["begin", "outimage", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "nested_terminated_by_end",
        file: "nested_terminated_by_end.sim",
        tokens: &["begin", "begin", "end", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "nested_end_comment_if",
        file: "nested_end_comment_if.sim",
        tokens: &["begin", "begin", "end", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "else_stops_comment",
        file: "else_stops_comment.sim",
        tokens: &[
            "begin", "if", "true", "then", "begin", "outtext", ";", "end", "else", "outtext", ";",
            "end",
        ],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "else_with_comment",
        file: "else_with_comment.sim",
        tokens: &[
            "begin", "if", "true", "then", "begin", "outtext", ";", "end", "else", "outtext", ";",
            "end",
        ],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "when_stops_comment",
        file: "when_stops_comment.sim",
        tokens: &[
            "begin",
            "inspect",
            "B",
            "when",
            "B1",
            "do",
            "begin",
            "outimage",
            ";",
            "end",
            "when",
            "B2",
            "do",
            "S2",
            "otherwise",
            "S3",
            ";",
            "end",
        ],
        absent_idents: &[],
        compiles: Some(false),
    },
    Case {
        name: "otherwise_stops_comment",
        file: "otherwise_stops_comment.sim",
        tokens: &[
            "begin",
            "inspect",
            "B",
            "when",
            "B1",
            "do",
            "begin",
            "outimage",
            ";",
            "end",
            "otherwise",
            "S3",
            ";",
            "end",
        ],
        absent_idents: &[],
        compiles: Some(false),
    },
    Case {
        name: "end_of_file",
        file: "end_of_file.sim",
        tokens: &["begin", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "end_otherwis",
        file: "end_otherwis.sim",
        tokens: &["begin", "end", ";"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "end_x",
        file: "end_x.sim",
        tokens: &["begin", "end", ";"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "semicolon_terminates_comment_outtext_kept",
        file: "semicolon_terminates_comment_outtext_kept.sim",
        tokens: &["begin", "begin", "end", ";", "outtext", ";", "end"],
        absent_idents: &[],
        compiles: Some(true),
    },
    Case {
        name: "semicolon_on_next_line_swallows_outtext",
        file: "semicolon_on_next_line_swallows_outtext.sim",
        tokens: &["begin", "begin", "end", ";", "end"],
        absent_idents: &["outtext"],
        compiles: Some(true),
    },
    Case {
        name: "weekend_in_comment",
        file: "weekend_in_comment.sim",
        // Single identifier after `end` is the §4.10.4 block-end name (emitted),
        // not end-comment text — matching `end myblock;`.
        tokens: &[
            "begin",
            "procedure",
            "Have_weekend",
            ";",
            "begin",
            "end",
            "Have_weekend",
            ";",
            "end",
            ";",
        ],
        absent_idents: &[],
        compiles: Some(true),
    },
];

#[test]
fn end_comment_fixtures_match_expected_tokens() {
    for case in CASES {
        let source = common::fixture(&format!("end-comment/{}", case.file));
        let kinds = tokenize(&source);
        let got = significant_tokens(&kinds);
        let expected: Vec<String> = case.tokens.iter().map(|token| (*token).into()).collect();

        assert_eq!(got, expected, "token mismatch for {}", case.name);

        for ident in case.absent_idents {
            assert!(
                !has_identifier(&kinds, ident),
                "identifier '{ident}' should be swallowed by end-comment in {}",
                case.name
            );
        }
    }
}

#[test]
fn end_comment_fixtures_compile_as_expected() {
    for case in CASES {
        let Some(should_compile) = case.compiles else {
            continue;
        };

        let source = common::fixture(&format!("end-comment/{}", case.file));
        let did_compile = outimage::compile_str(&source).is_ok();

        assert_eq!(
            did_compile, should_compile,
            "compile expectation for {}",
            case.name
        );
    }
}

// ---- Tests not covered by tree-sitter fixtures (kept deliberately) ----

#[test]
fn end_comment_terminators_are_case_insensitive() {
    let kinds = tokenize("begin END of program;");
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::End),
            TokenKind::Semicolon,
        ]
    ));
}

#[test]
fn direct_comment_takes_precedence_over_end_comment() {
    let kinds = tokenize("begin ! this mentions end; end;");
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::End),
            TokenKind::Semicolon,
        ]
    ));
}

#[test]
fn comment_keyword_body_may_contain_end_and_else() {
    let kinds = tokenize("begin comment end else; end;");
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::End),
            TokenKind::Semicolon,
        ]
    ));
}

#[test]
fn standard_example_end_bang_then_separates_else() {
    let kinds = tokenize("begin end !then; else");
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::End),
            TokenKind::Semicolon,
            TokenKind::Keyword(Keyword::Else),
        ]
    ));
}

#[test]
fn infamous_case_parses_as_if_else() {
    // §1.8.2: `end !then; else` — `then` is inside the end-comment, so `else`
    // binds to the outer `if` (counterintuitive but correct Simula semantics).
    assert!(
        outimage::compile_str("begin if true then begin end !then; else begin end; end;").is_ok()
    );
}
