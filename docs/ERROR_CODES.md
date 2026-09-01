# Diagnostic error codes

*sim* diagnostics use stable numbered codes, rendered by
[ariadne](https://codeberg.org/zesterer/ariadne). Family aliases (`E-lex`, …)
still work with `sim explain` and list the catalogued members.

| Range | Group | Typical causes |
| --- | --- | --- |
| `E00xx` | Lex | Unexpected character, bad string/number, missing separator |
| `E01xx` | Parse | Unexpected token, missing `end`, `:=` vs `:-`, incomplete `short`/`long`/`ref` |
| `E02xx` | Types | Assignment mismatch, `if` condition, operators, arity, conditional branches, array bounds |
| `E03xx` | Names | Unknown procedure/attribute/name, undefined label/switch, duplicate declaration |
| `E04xx` | Classes | `hidden` / `protected`, prefix cycle, virtual match, `this` / `qua` / `is` / `in` |
| `E05xx` | Parameters | Illegal transmission mode, duplicate formals, formal procedure actuals |
| `E06xx` | Externals | Unknown kind, missing specification, non-empty body, foreign ABI |
| `E07xx` | Lowering | Simulation not active, unsupported-on-this-backend |
| `E08xx` | Link / toolchain | Host linker missing or failed; object writer / SDK |
| `E09xx` | Runtime | Bounds, none-ref, division by zero, undefined power (not a compiler bug) |
| `W0001` | Unused | Local / parameter / label never referenced (`sim check`; alias `W-unused`) |
| `I0001` | ICE | Internal compiler invariant; not a user mistake |

## Catalogued codes

Generated from `src/diagnostics/catalog.rs` (`catalog_index_markdown()`). Do not edit the table by hand — a test fails if it drifts.

| Code | Title | Group |
| --- | --- | --- |
| `E0001` | UNEXPECTED CHARACTER | E-lex |
| `E0002` | UNTERMINATED STRING | E-lex |
| `E0003` | MISSING SEPARATOR | E-lex |
| `E0004` | DIRECTIVE PLACEMENT | E-lex |
| `E0005` | INVALID NUMBER | E-lex |
| `E0006` | INVALID ISO CODE | E-lex |
| `E0101` | UNEXPECTED TOKEN | E-parse |
| `E0102` | UNEXPECTED END OF FILE | E-parse |
| `E0103` | MISSING END | E-parse |
| `E0104` | WRONG ASSIGNMENT | E-parse |
| `E0105` | INCOMPLETE TYPE | E-parse |
| `E0201` | TYPE MISMATCH | E-semantic |
| `E0202` | WRONG ASSIGNMENT | E-semantic |
| `E0203` | WRONG ASSIGNMENT | E-semantic |
| `E0204` | TYPE MISMATCH | E-semantic |
| `E0205` | WRONG NUMBER OF ARGUMENTS | E-semantic |
| `E0206` | TYPE MISMATCH | E-semantic |
| `E0301` | UNKNOWN NAME | E-semantic |
| `E0302` | UNKNOWN NAME | E-semantic |
| `E0303` | UNKNOWN ATTRIBUTE | E-semantic |
| `E0304` | UNDEFINED LABEL | E-semantic |
| `E0305` | UNDEFINED SWITCH | E-semantic |
| `E0306` | DUPLICATE DECLARATION | E-semantic |
| `W0001` | UNUSED | W-unused |
| `E0901` | ARRAY TOO LARGE | E-runtime |
| `E0902` | NONE REFERENCE | E-runtime |
| `E0903` | UNDEFINED LABEL | E-runtime |
| `E0904` | ARRAY INDEX | E-runtime |
| `E0905` | DIVISION BY ZERO | E-runtime |
| `E0906` | UNDEFINED POWER | E-runtime |
| `E0401` | PROTECTED ATTRIBUTE | E-semantic |
| `E0402` | HIDDEN ATTRIBUTE | E-semantic |
| `E0501` | ILLEGAL MODE | E-semantic |
| `E0207` | ARRAY BOUNDS | E-semantic |
| `E0208` | ARRAY BOUND | E-semantic |
| `E0209` | EMPTY SWITCH | E-semantic |
| `E0210` | NOT AN EXPRESSION | E-semantic |
| `E0211` | CONSTANT | E-semantic |
| `E0212` | CONSTANT | E-semantic |
| `E0403` | PREFIX CYCLE | E-semantic |
| `E0404` | UNKNOWN CLASS | E-semantic |
| `E0405` | PREFIX SCOPE | E-semantic |
| `E0406` | VIRTUAL MISMATCH | E-semantic |
| `E0407` | ILLEGAL THIS | E-semantic |
| `E0408` | NOT A PREFIX | E-semantic |
| `E0409` | HIDDEN | E-semantic |
| `E0410` | DUPLICATE VIRTUAL | E-semantic |
| `E0411` | DETACH | E-semantic |
| `E0412` | ATTRIBUTE SCOPE | E-semantic |
| `E0502` | DUPLICATE FORMAL | E-semantic |
| `E0503` | FORMAL NAME | E-semantic |
| `E0504` | FORMAL REDECLARED | E-semantic |
| `E0505` | ARRAY ARITY | E-semantic |
| `E0506` | FORMAL SCOPE | E-semantic |
| `E0507` | FORMAL PROCEDURE | E-semantic |
| `E0601` | EXTERNAL KIND | E-semantic |
| `E0602` | EXTERNAL SPEC | E-semantic |
| `E0603` | EXTERNAL BODY | E-semantic |
| `E0604` | FOREIGN BOUNDARY | E-semantic |
| `E0701` | NO SIMULATION | E-codegen |
| `E0702` | NOT LOWERED | E-codegen |
| `E0801` | LINKER FAILED | E-codegen |
| `E0802` | LINKER NOT FOUND | E-codegen |
| `E0803` | TOOLCHAIN | E-codegen |
| `I0001` | INTERNAL ERROR | I-internal |

Colour is controlled by `--color auto|always|never` and `NO_COLOR` / `FORCE_COLOR`.

### Tooling

- `sim explain E0201` — full essay for a numbered code
- `sim explain E-semantic` — family blurb plus catalogued `E02xx`–`E06xx` codes
- `sim --json …` — one JSON object per diagnostic (`code`, `title`, `phase`,
  `message`, `span`, `notes`, `helps`, `suggestions`, `params`, `related`)
- `sim --compact …` — one line: `E0201 TYPE MISMATCH: file:line:col: …`
- `sim --explain-errors=short …` — snippet and labels, without notes/helps
- `sim check file.sim` — lenient parse + semantic + MIR lower; prints `W0001` unused warnings (disable with `--no-unused`)

See [`ERROR_MESSAGE_PLAN.md`](../ERROR_MESSAGE_PLAN.md) for the quality bar and
migration plan. Adding a user-facing error requires a catalog row (`DiagId`) and
a constructor in `src/diagnostics/report.rs` — do not `format!` English at the
call site.

Long-form pages for each numbered code live in [`docs/diagnostics/`](diagnostics/README.md).
Confusing reports are bugs: label them `needs-better-error`.
