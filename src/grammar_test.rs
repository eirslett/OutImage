//! Generate vscode-tmgrammar-test unit files from the Simula lexer.
//!
//! Token classification follows the compiler's lexer, with a small amount of
//! parser-shaped lookaround (`ref(Class)`, `name begin`, `new`/`qua` class
//! identifiers) so the emitted scopes are a spec for the TextMate grammar.

use crate::error::CompileError;
use crate::lex::highlight::{HighlightSpan, highlight_source};

const SCOPE_NAME: &str = "source.simula";
const COMMENT_TOKEN: &str = "//";

/// Strip a vscode-tmgrammar-test header and assertion lines, if present.
pub fn extract_source(input: &str) -> (String, Option<String>) {
    let first_line = input.lines().next().unwrap_or("");
    let Some((comment_token, description)) = parse_syntax_test_header(first_line) else {
        return (normalize_newlines(input), None);
    };

    let rest = input
        .strip_prefix(first_line)
        .and_then(|s| s.strip_prefix('\n').or_else(|| s.strip_prefix("\r\n")))
        .unwrap_or("");

    let mut source_lines = Vec::new();
    for line in split_lines(rest) {
        if is_assertion_line(line, comment_token) {
            continue;
        }
        source_lines.push(line);
    }
    (source_lines.join("\n"), description)
}

/// Render a vscode-tmgrammar-test unit file for `input`.
pub fn render_syntax_test(input: &str, description: Option<&str>) -> Result<String, CompileError> {
    let (source, header_description) = extract_source(input);
    let description = description
        .or(header_description.as_deref())
        .unwrap_or("generated");
    let spans = highlight_source(&source)?;
    Ok(render_file(&source, &spans, description))
}

fn render_file(source: &str, spans: &[HighlightSpan], description: &str) -> String {
    let mut out = format!("{COMMENT_TOKEN} SYNTAX TEST \"{SCOPE_NAME}\" \"{description}\"\n");
    let lines = line_ranges(source);
    for (line_start, line) in &lines {
        out.push_str(line);
        out.push('\n');
        for span in line_spans(*line_start, line, spans) {
            out.push_str(&render_assertion(
                span.span.start,
                span.span.end,
                span.scope,
            ));
            out.push('\n');
        }
    }
    out
}

fn line_spans(line_start: usize, line: &str, spans: &[HighlightSpan]) -> Vec<HighlightSpan> {
    let line_end = line_start + line.len();
    let mut clipped: Vec<HighlightSpan> = Vec::new();
    for span in spans {
        let start = span.span.start.max(line_start);
        let end = span.span.end.min(line_end);
        if start >= end {
            continue;
        }
        let rel_start = char_index(line, start - line_start);
        let rel_end = char_index(line, end - line_start);
        if rel_start >= rel_end {
            continue;
        }
        if let Some(last) = clipped.last_mut()
            && last.scope == span.scope
            && last.span.end == rel_start
        {
            last.span.end = rel_end;
            continue;
        }
        clipped.push(HighlightSpan {
            span: rel_start..rel_end,
            scope: span.scope,
        });
    }
    clipped
}

fn char_index(line: &str, byte_index: usize) -> usize {
    let byte_index = byte_index.min(line.len());
    line[..byte_index].chars().count()
}

fn render_assertion(start: usize, end: usize, scope: &str) -> String {
    let comment_len = COMMENT_TOKEN.len();
    if start < comment_len {
        format!(
            "{COMMENT_TOKEN} <{}{} {scope}",
            "~".repeat(start),
            "-".repeat(end - start)
        )
    } else {
        format!(
            "{COMMENT_TOKEN}{}{} {scope}",
            " ".repeat(start - comment_len),
            "^".repeat(end - start)
        )
    }
}

fn parse_syntax_test_header(line: &str) -> Option<(&str, Option<String>)> {
    let mut parts = line.split_whitespace();
    let comment_token = parts.next()?;
    if !parts.next().is_some_and(|word| word == "SYNTAX") {
        return None;
    }
    if !parts.next().is_some_and(|word| word == "TEST") {
        return None;
    }
    let remainder = line
        .split_once("TEST")
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    let mut scopes = remainder.split('"').filter(|s| !s.trim().is_empty());
    let _scope = scopes.next()?;
    let description = scopes.next().map(str::to_string);
    Some((comment_token, description))
}

fn is_assertion_line(line: &str, comment_token: &str) -> bool {
    let Some(rest) = line.strip_prefix(comment_token) else {
        return false;
    };
    let rest = rest.trim_start();
    rest.starts_with('^') || rest.starts_with('<')
}

fn normalize_newlines(input: &str) -> String {
    split_lines(input).join("\n")
}

fn split_lines(input: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, ch) in input.char_indices() {
        if ch == '\n' {
            let mut line = &input[start..index];
            if let Some(stripped) = line.strip_suffix('\r') {
                line = stripped;
            }
            lines.push(line);
            start = index + 1;
        }
    }
    let mut last = &input[start..];
    if let Some(stripped) = last.strip_suffix('\r') {
        last = stripped;
    }
    lines.push(last);
    lines
}

fn line_ranges(source: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, ch) in source.char_indices() {
        if ch == '\n' {
            let mut line = &source[start..index];
            if let Some(stripped) = line.strip_suffix('\r') {
                line = stripped;
            }
            lines.push((start, line));
            start = index + 1;
        }
    }
    let mut last = &source[start..];
    if let Some(stripped) = last.strip_suffix('\r') {
        last = stripped;
    }
    lines.push((start, last));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> String {
        render_syntax_test(source, Some("sample")).expect("lex")
    }

    #[test]
    fn renders_begin_type_and_end() {
        let output = render("begin\n    integer a;\nend;\n");
        assert_eq!(
            output,
            r#"// SYNTAX TEST "source.simula" "sample"
begin
// <----- keyword.control.begin
    integer a;
//  ^^^^^^^ storage.type
//          ^ variable
//           ^ punctuation.terminator.statement
end;
// <--- keyword.control.end
// ^ punctuation.terminator.statement

"#
        );
    }

    #[test]
    fn renders_comments_and_directives() {
        let output = render("% directive\n! a comment;\ncomment also;\n-- line comment\n");
        assert!(output.contains("comment.directive"));
        assert!(output.contains("comment.block"));
        assert!(output.contains("% directive"));
        assert!(output.contains("! a comment;"));
        assert!(output.contains("-- line comment"));
    }

    #[test]
    fn renders_end_comment() {
        let output = render("begin end-this is a comment;\n");
        assert!(output.contains("keyword.control.end"));
        assert!(output.contains("comment.block"));
    }

    #[test]
    fn block_end_name_is_an_end_comment() {
        let output = render("begin end trace;\n");
        assert!(output.contains("keyword.control.end"));
        assert!(output.contains("comment.block"));
        assert!(!output.contains("variable"));
    }

    #[test]
    fn renders_ref_qualification_as_class_name() {
        let output = render("begin ref(BaseClass) something; end;\n");
        assert!(output.contains("storage.modifier"));
        assert!(output.contains("entity.name.class"));
        assert!(output.contains("variable"));
    }

    #[test]
    fn treats_unary_minus_as_an_operator() {
        let output = render("begin a := -1; x := minreal+1; end;\n");
        assert!(output.contains("constant.numeric.decimal"));
        assert!(output.contains("keyword.operator.assignment"));
        assert!(output.contains("keyword.operator.arithmetic"));
    }

    #[test]
    fn uses_specific_control_keyword_scopes() {
        let output = render("begin activate X; if true then false else true; end;\n");
        assert!(output.contains("keyword.control.activate"));
        assert!(output.contains("keyword.control.if"));
        assert!(output.contains("keyword.control.then"));
        assert!(output.contains("keyword.control.else"));
        assert!(!output.contains("keyword.control\n"));
    }

    #[test]
    fn classifies_radix_and_decimal_numbers() {
        let output = render("begin a := 16RFFFE; a := 2.0&+1; a := &3; a := &&+01; end;\n");
        assert!(output.contains("constant.numeric.radix"));
        assert!(output.contains("constant.numeric.decimal"));
        assert!(
            !render("begin a := &3; end;\n").contains("keyword.operator.arithmetic"),
            "exponent-only `&3` is one number, not an operator"
        );
        let unary = render("begin a := -&2; end;\n");
        assert!(unary.contains("keyword.operator.arithmetic"));
        assert!(unary.contains("constant.numeric.decimal"));
    }

    #[test]
    fn classifies_strings_characters_and_bools() {
        let output = render("begin str :- \"hi\"; c := 'A'; b := true; end;\n");
        assert!(output.contains("string.quoted.double"));
        assert!(output.contains("constant.character"));
        assert!(output.contains("constant.language.bool"));
    }

    #[test]
    fn strips_existing_syntax_test_assertions() {
        let (source, description) = extract_source(
            "// SYNTAX TEST \"source.simula\" \"1 basics\"\nbegin\n// <---- keyword.control.begin\nend\n",
        );
        assert_eq!(source, "begin\nend\n");
        assert_eq!(description.as_deref(), Some("1 basics"));
    }

    #[test]
    fn named_block_prefix_is_a_class_name() {
        let output = render("Main begin end;\n");
        assert!(output.contains("entity.name.class"));
        assert!(output.contains("keyword.control.begin"));
    }

    #[test]
    fn virtual_spec_is_not_a_label() {
        let output = render("begin virtual: procedure virtproc; end;\n");
        assert!(output.contains("keyword.other.virtual"));
        assert!(!output.contains("entity.name.label"));
    }

    #[test]
    fn begin_after_do_stays_begin() {
        let output = render("begin while false do begin end; end;\n");
        assert!(output.contains("keyword.control.do"));
        assert!(output.matches("keyword.control.begin").count() >= 2);
    }

    #[test]
    fn outtext_is_an_environment_procedure() {
        let output = render("begin outtext(str); end;\n");
        assert!(output.contains("entity.name.function"));
    }

    #[test]
    fn array_bounds_colon_is_not_a_label() {
        let output = render("begin\n    real array X(P:1);\n    L: X(1) := 0;\nend;\n");
        let bound_line = output
            .split("real array X(P:1);")
            .nth(1)
            .unwrap()
            .split("L:")
            .next()
            .unwrap();
        assert!(
            !bound_line.contains("entity.name.label"),
            "array bound P must not be a label: {bound_line}"
        );
        assert!(output.contains("entity.name.label"));
        assert!(output.contains("variable"));
    }
}
