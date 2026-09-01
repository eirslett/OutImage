# Changelog — Simula (sim) VS Code extension

All notable changes to `editors/vscode` are documented here.

## [Unreleased]

### Added

- `--` line comments (through end of line), matching the compiler extension
- `simula.allowDoubleDashComments` (default true) forwarded to LSP and DAP
- **Compile Current File** command; `sim` tasks resolve through the same launcher

### Changed

- Language server binary comes only from `simula.languageServerPath` (no workspace `target/` auto-detect)
- On first run, a missing configured path is resolved via `PATH` and written back to settings
- Missing-binary warning includes a **Retry** action (after installing sim)
- Removed `simula.autoDetectWorkspaceBinary` and `simula.preferDebugBinary`

## [0.1.0] - 2026-07-28

### Added

- Modular extension (`boot`, `client`, `binary`, `commands`, `notify`, `trust`, `settings`)
- Workspace trust gating; debounced config reload; boot mutex
- Commands: open documentation, check/run current file, problems-panel explain
- Status bar server version tooltip; gallery banner / preview manifest flags
- Marketplace icon (`icons/simula.png`)
- Grammar: `ref(Class)`, `comment` blocks, procedure/class headings
- Language configuration: `wordPattern`, indentation and onEnter rules
- Snippets: switch, external, object generator
- Walkthrough: Get Started with Simula
- Contributed `sim` task definition + `$sim` ariadne problem matcher
- `initializationOptions` / live sync: `allowSquareBracketSubscripts`,
  `checkOn`, `debounceMs`
- Unit tests (7), ESLint, CI lint + e2e smoke (`npm run test:e2e`)
- E2E diagnostics via `test/fixtures/mock-lsp.js` (node path from `SIM_E2E_NODE`)
- Robust `stopLanguageClient` when prior start failed (missing binary)
- CI packages and uploads `.vsix` on `v*` / `vscode-v*` tags
- Interpreter debugger (`type: sim` → `sim dap`): breakpoints, step,
  locals, expandables, Simulation SQS, CodeLLDB native launch snippet,
  optional `backend: "native"` + `nativeProgram` hand-off
- `PUBLISH.md` for marketplace / air-gapped install
