# Simula language server (`sim lsp`)

The `sim` binary includes a [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) backend for `.sim` files. It runs over stdio and is suitable for Neovim, Helix, VS Code / Cursor, Zed, Emacs, and other LSP clients.

## Quick start

Build the compiler, then point your editor at the `sim lsp` subcommand:

```bash
cargo build --release
./target/release/sim lsp
```

The server advertises language id **`simula`**. Map `*.sim` buffers to that id in your client.

## Features

| Capability | Status |
|------------|--------|
| Diagnostics (lex / parse / semantic; optional MIR) | Yes — push + pull; debounced ~200ms on change; **partial AST** on syntax errors (`parse_lenient`) |
| Hover | Yes — symbols, keywords, ENVIRONMENT builtins (markdown/plain) |
| Document symbols (outline) | Yes — hierarchical or flat per client |
| Folding ranges | Yes — procedure / class / block bodies |
| Go to definition / type definition / implementation | Yes — locals, procs, classes, remotes, virtual matches |
| Find references / document highlight | Yes — open doc + workspace index |
| Call hierarchy / type hierarchy | Yes |
| Completions | Yes — scope, keywords, builtins, `.` attrs, snippets; `isIncomplete` |
| Signature help | Yes — procedure calls |
| Rename / prepare rename | Yes |
| Linked editing | Yes — symbol refs / named blocks |
| Code lens | Yes — reference counts on procs/classes |
| Semantic tokens | Yes — full + range + delta |
| Workspace symbols | Yes — open docs + sandboxed folder index |
| Selection range | Yes |
| Formatting / range formatting | Yes |
| Code actions | Yes — explain codes; keyword typo fix; missing `end`/`;`; suggest `external` |
| Inlay hints | Yes — parameter names at calls; types on class/procedure headings |
| On-type formatting | Yes — after `;` / `end` |
| Configuration | Yes — push + pull (`checkOn`, `debounceMs`, `allowSquareBracketSubscripts`, `allowDoubleDashComments`, `enableMirCheck`, `enableUnusedLints`, `enableHeadingTypeInlayHints`, `maxDocumentBytes`) |

Workspace folders are indexed for `.sim` files under registered roots only (path sandbox). Indexing reports `workDoneProgress` and publishes diagnostics for **closed** indexed files only (open buffers keep the live, versioned publish). Cross-file definition / references / external import suggestions use that index. Cross-file rename remains best-effort.

Diagnostics follow the same freshness rule as rust-analyzer snapshots and clangd's ASTWorker: a slow analysis of an older edit is discarded if the buffer has moved on. `publishDiagnostics` always includes the document version; pull diagnostics are served only when the snapshot still matches the live text.

While typing, the server uses **`parse_lenient`**: missing `end`, broken statements, and truncated declarations still produce a best-effort AST so hover / goto / completions keep working on the recovered parts. Lex recovery skips illegal characters and keeps tokenizing so several `E0001`s can appear at once. CLI `sim check` / compile stay on the strict fail-fast parser (but still bundle recovered lex errors).

## Threat model (brief)

- The server only **reads** `.sim` files under workspace folders (or open buffers the client sends).
- Code actions and code lenses do **not** execute shell or editor commands.
- Oversize buffers are refused (`maxDocumentBytes`, default 2 MiB).
- Analysis / request handlers are wrapped in `catch_unwind` so panics do not abort the process.

## VS Code / Cursor extension

A thin extension lives in [`editors/vscode`](../editors/vscode/README.md). It registers `simula` for `*.sim`, provides syntax highlighting, and starts `sim lsp`.

```bash
cargo build --release
cd editors/vscode && npm install && npm run compile
```

Then press **F5** in VS Code with `editors/vscode` open, or install the generated `.vsix` (`npm run package`).

Settings use the `simula.*` prefix (see the extension README).

## Other editors

### Neovim (`nvim-lspconfig`)

```lua
vim.lsp.enable('sim')
-- or manual:
-- vim.lsp.config('sim', {
--   cmd = { '/path/to/sim', 'lsp' },
--   filetypes = { 'simula' },
--   root_markers = { '.git' },
-- })
```

### Helix

```toml
[[language]]
name = "simula"
language-servers = ["sim"]

[language-server.sim]
command = "sim"
args = ["lsp"]
```

### Zed

Add to your Zed settings (or a project `.zed/settings.json`):

```json
{
  "languages": {
    "Simula": {
      "language_servers": ["sim"]
    }
  },
  "lsp": {
    "sim": {
      "binary": {
        "path": "sim",
        "arguments": ["lsp"]
      }
    }
  }
}
```

Map `*.sim` to language id `Simula` / `simula` as required by your Zed build.

### Emacs (`eglot`)

```elisp
(add-to-list 'eglot-server-programs
             '(simula-mode . ("sim" "lsp")))
```

## Diagnostics

Errors use OutImage report codes (`E0201`, `E-lex`, …). Unused locals surface as warnings (`W0001`, alias `W-unused`) with the `Unnecessary` tag when `enableUnusedLints` is on (default). The same warnings are emitted by `sim check` (disable with `--no-unused`). Catalogued diagnostics with a replacement suggestion also offer a lightbulb quick-fix. Use **Explain** in the editor, or run:

```bash
sim explain E-semantic
```

See [ERROR_CODES.md](ERROR_CODES.md) for the full table.

Optional MIR check (more expensive): set `simula.enableMirCheck` / `enableMirCheck: true` in init options.

## Development

- Tests: `cargo test --lib lsp::`
- Protocol smoke tests live in `src/lsp/server.rs`
- Trace: set `SIM_LSP_TRACE=1` for stderr analysis logs
