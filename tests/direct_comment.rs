//! Direct-comment fixtures ported from tree-sitter Simula corpus (§1.8.2 / §1.10).
//!
//! Each case lives in `tests/fixtures/direct-comment/` and is validated for
//! tokenization and compilation.

mod common;

use outimage::lex::{Keyword, TokenKind};
use outimage::source::SourceFile;

struct Case {
    name: &'static str,
    file: &'static str,
}

const CASES: &[Case] = &[
    Case {
        name: "empty_shorthand",
        file: "empty_shorthand.sim",
    },
    Case {
        name: "empty_keyword",
        file: "empty_keyword.sim",
    },
    Case {
        name: "simple_shorthand",
        file: "simple_shorthand.sim",
    },
    Case {
        name: "simple_keyword",
        file: "simple_keyword.sim",
    },
    Case {
        name: "multiline_shorthand",
        file: "multiline_shorthand.sim",
    },
    Case {
        name: "multiline_keyword",
        file: "multiline_keyword.sim",
    },
];

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

#[test]
fn direct_comment_fixtures_strip_to_begin_end() {
    for case in CASES {
        let source = common::fixture(&format!("direct-comment/{}", case.file));
        let got = significant_tokens(&tokenize(&source));

        assert_eq!(
            got,
            vec!["begin".to_string(), "end".to_string()],
            "token mismatch for {}",
            case.name
        );
    }
}

#[test]
fn direct_comment_fixtures_compile() {
    for case in CASES {
        let source = common::fixture(&format!("direct-comment/{}", case.file));
        outimage::compile_str(&source)
            .unwrap_or_else(|error| panic!("fixture {} should compile: {error}", case.name));
    }
}

#[test]
fn comment_keyword_is_case_insensitive() {
    let kinds = tokenize("begin COMMENT abc; end;");
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
fn comment_keyword_is_alternate_direct_comment_shorthand() {
    assert_eq!(
        tokenize("begin ! note; end;"),
        tokenize("begin comment note; end;")
    );
}

#[test]
fn direct_comment_body_may_contain_end_and_else() {
    let kinds = tokenize("begin ! end else; end;");
    assert!(matches!(
        kinds.as_slice(),
        [
            TokenKind::Keyword(Keyword::Begin),
            TokenKind::Keyword(Keyword::End),
            TokenKind::Semicolon,
        ]
    ));
}
