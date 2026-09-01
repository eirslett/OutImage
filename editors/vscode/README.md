# Simula (sim) — VS Code extension

Production-oriented [VS Code](https://code.visualstudio.com/) / [Cursor](https://cursor.com/) extension for Simula (`.sim`).

See [`docs/LSP.md`](../../docs/LSP.md) (server capabilities) and [`PUBLISH.md`](PUBLISH.md) (release).

## Features

- **Language** registration (`simula`) with syntax highlighting, indentation rules, and snippets
- **LSP** via `sim lsp` — diagnostics, hover, completion, rename, format, semantic tokens, type definition, remote-attribute navigation, etc.
- **Workspace trust** — language server starts only in trusted workspaces
- **Commands:** restart server, show output, explain diagnostic, open docs, check/run/compile current file
- **Tasks:** `sim` task type with `$sim` problem matcher (ariadne `╭─[file:line:col]` output)
- **Status bar** with server state and version tooltip
- **Get Started** walkthrough (`Simula: Get Started with Simula` in walkthroughs view)

## Prerequisites

A `sim` executable on your `PATH` (or set `simula.languageServerPath`).

Build from this repository:

```bash
cargo build --release   # from repo root
```

Then set `simula.languageServerPath` to `target/release/sim`, or put that binary on your `PATH`.

Node.js 20+ for building the extension.

## Development

```bash
cd editors/vscode
npm install
npm run compile
npm test
npm run lint
```

Press **F5** (or open `simula.code-workspace` and run the extension host).

End-to-end smoke (downloads VS Code):

```bash
npm run test:e2e
```

## Debugging (interpreter)

The extension contributes debugger type `sim`, backed by `sim dap`
(statement-level interpreter stepping).

1. Open a `.sim` file
2. Set breakpoints in the gutter (optional; default stops on entry)
3. Run **Debug: Start Debugging** (F5) and choose **Debug Simula (interpreter)**

Requires a resolved `sim` binary (same as the language server).

### Native / CodeLLDB (optional)

For AOT binaries compiled with `sim compile -g`, install CodeLLDB and use a
`type: lldb` launch config (macOS / Linux / Windows DWARF-in-PE) — see
`docs/DEBUG.md`. Prefer the contributed `sim` debugger for day-to-day
Simula stepping. Visual Studio PDB is not emitted yet.

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `simula.languageServerPath` | `sim` | Executable (`${workspaceFolder}` supported). First run searches `PATH` if missing. |
| `simula.languageServerArgs` | `["lsp"]` | Arguments passed when starting the language server |
| `simula.trace.server` | `off` | LSP trace level |
| `simula.allowSquareBracketSubscripts` | `true` | Passed to LSP (init + live config sync) |
| `simula.allowDoubleDashComments` | `true` | Passed to LSP (init + live config sync). `--` is a line comment when true. |
| `simula.checkOn` | `change` | When to diagnose: `open` / `change` / `save` |
| `simula.debounceMs` | `200` | Debounce for change-triggered analysis |

## Semantic highlighting

Colors come from the active color theme. Simula classifies tokens with language-specific types (`type` for `integer`/`procedure`, `boolean` for `true`, `character` for `'A'`, `parenthesis`/`semicolon` for punctuation, plus the usual `keyword`/`class`/`function`/`variable`). Themes color those types; you can override a type for Simula only in `settings.json`. Reload the extension host after changing the language server if an open `.sim` file still shows the old classification.

To override a token type for Simula only, add to `settings.json`:

```json
{
  "editor.semanticTokenColorCustomizations": {
    "[Default Dark+]": {
      "rules": {
        "class.simula": "#4EC9B0",
        "function.simula": "#DCDCAA",
        "variable.simula": "#9CDCFE"
      }
    }
  }
}
```

## Tasks

Example `.vscode/tasks.json` entry (uses the contributed `$sim` problem matcher):

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "sim: check current file",
      "type": "shell",
      "command": "sim",
      "args": ["check", "${file}"],
      "group": "build",
      "problemMatcher": ["$sim"]
    },
    {
      "label": "sim: run current file",
      "type": "shell",
      "command": "sim",
      "args": ["run", "${file}"],
      "group": "test",
      "problemMatcher": []
    }
  ]
}
```

Prefer the language server Problems panel for day-to-day editing. Contributed
`type: "sim"` tasks use the same `sim` executable as LSP/DAP:

```json
{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "sim: check current file",
      "type": "sim",
      "command": "check",
      "args": ["${file}"],
      "group": "build",
      "problemMatcher": ["$sim"]
    }
  ]
}
```

## Package

```bash
npm run package
```

Install the `.vsix` via Extensions → `...` → **Install from VSIX**.

## License

MIT — see [LICENSE](LICENSE).
