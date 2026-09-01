# Debugging Simula with sim

## VS Code / Cursor (interpreter — recommended)

1. Build `sim` (`cargo build` / `cargo build --release`).
2. Open a `.sim` file with the Simula extension.
3. **OutImage: Debug Current File** or F5 → **Debug Simula (interpreter)**.

Uses `sim dap`: statement stepping, breakpoints, conditionals, logpoints,
expandable objects, Simulation SQS scope, and exception breaks. Works on
macOS, Linux, and Windows (stdio DAP). Launch arguments include
`allowSquareBracketSubscripts` and `allowDoubleDashComments` (both default
true; set the latter false to parse `--` as two minus operators).

Optional launch.json `backend`:

| Value | Behavior |
| --- | --- |
| `interpreter` (default) | `sim dap` |
| `native` | Hands off to CodeLLDB using `nativeProgram` (requires `vadimcn.vscode-lldb`). On Windows, prefer interpreter DAP day-to-day; native uses DWARF-in-PE (not Visual Studio PDB yet). |

## CLI (`sim debug`)

Human-facing sugar over the same interpreter probe (no DAP client needed):

```bash
sim debug --break 12 --command 'print x' --command continue prog.sim
sim debug --stop-on-entry prog.sim   # then type: next / continue / locals / quit
```

Commands: `continue` (`c`), `next` (`n`), `step` (`s`), `out` (`o`),
`locals` (`l`), `print <expr>`, `backtrace` (`bt`), `quit` (`q`), `help`.

For IDE integration prefer `sim dap` (stdio DAP). `--trace` logs each stop
to stderr.

## Native binaries (AOT)

Compile with `-g` for DWARF. Locals are typed for scalars, `text`
(`SimrtTextFrame*`), arrays (`SimrtArrayI64*` / `SimrtArrayText*`), and
`ref(C)` (pointer to class structure); class members carry the same types when
known.

| Host | Debug artifact | Debugger |
| --- | --- | --- |
| macOS | DWARF in `.dSYM` companion | lldb / CodeLLDB |
| Linux | DWARF sections in the ELF | gdb or lldb / CodeLLDB |
| Windows | DWARF sections in the PE (`/DEBUG:DWARF`) | CodeLLDB (recommended); interpreter DAP for Simula stepping |

Prefer the interpreter debugger for day-to-day Simula. Use native when hunting
codegen issues. **PDB / CodeView** (Visual Studio `cppvsdbg`) is not emitted yet.

### macOS (lldb + dSYM)

```bash
sim compile -g prog.sim -o prog
lldb ./prog
(lldb) b /absolute/path/to/prog.sim:3
(lldb) run
(lldb) frame variable t a p
```

### Linux (gdb)

```bash
sim compile -g prog.sim -o prog
gdb ./prog
(gdb) break prog.sim:3
(gdb) run
(gdb) info locals
```

### VS Code CodeLLDB (optional)

Install the [CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-lldb)
extension, then add a launch configuration (after `sim compile -g`):

```json
{
  "type": "lldb",
  "request": "launch",
  "name": "Debug Simula (native / CodeLLDB)",
  "program": "${workspaceFolder}/prog",
  "cwd": "${workspaceFolder}",
  "sourceMap": {
    "/absolute/path/to/prog.sim": "${workspaceFolder}/prog.sim"
  }
}
```

Prefer the interpreter debugger (`type: sim`) for day-to-day Simula
stepping. Use native CodeLLDB when hunting Cranelift/codegen issues the
interpreter does not reproduce.

## Wasm / Chrome DevTools

Compile with debug info (portable AOT — no host C linker):

```bash
sim compile -g --target wasm-browser prog.sim -o prog.wasm
# or: --target wasm-node
```

`-g` writes a Source Map v3 `.map`, embeds `sourceMappingURL`, and emits
DWARF-in-wasm `.debug_*` sections. Load the module in a page that hosts it,
open Chrome DevTools → **Sources**, and map back to the original `.sim` when
the browser supports the source map (and DWARF where available).

Browser Simulation / disk BASICIO remain limited — see [`docs/RUNTIME.md`](RUNTIME.md).

## Host portability

Windows native AOT requires MSVC libraries (`LIB`); open an x64 Native Tools
prompt or use `ilammy/msvc-dev-cmd` in CI. Text I/O keeps LF (not forced `\r\n`).
