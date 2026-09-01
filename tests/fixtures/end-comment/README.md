# End-comment fixtures

Simula sources ported from the tree-sitter Simula test corpus, validating
[Simula Standard §1.8.1](https://portablesimula.github.io/github.io/doc/SimulaStandard86/chap_1.htm)
end-comment behavior.

Each `.sim` file is exercised by `tests/end_comment.rs`, which checks:

- **Token stream** — significant tokens after end-comment text is stripped
- **Compile** — whether the current parser accepts the program (many cases are
  lex-only until `if`, `inspect`, and `procedure` parsing land)

## Redundancy with earlier unit tests

These fixture cases supersede the old inline tests that were removed:

| Fixture | Replaces |
| --- | --- |
| `trivial_semicolon_after_end` | `bare_end_comment_before_semicolon` |
| `end_of_file`, `end_x` | `end_comment_with_descriptive_text`, `end_comment_spans_lines` |
| `nested_terminated_by_end` | `end_comment_terminates_at_next_end_keyword` |
| `else_stops_comment`, `when_stops_comment`, `otherwise_stops_comment` | minimal `begin end note else/when/otherwise` tests |
| `end_otherwis` | `end_comment_does_not_trigger_on_elsewhere_substrings` |

Still tested separately (not in tree-sitter corpus):

- Case-insensitive terminators
- Direct comment (`!`) precedence
- `comment` keyword bodies
- Standard §1.8.2 `end !then; else` infamous case

## Parser limitations (compile expected to fail)

- `when_stops_comment`, `otherwise_stops_comment` — no `inspect` parser yet
- `weekend_in_comment` — no `procedure` declaration parser yet
- `trivial_semicolon_inside_block` — bare `;` is a dummy-statement (§4.11)
