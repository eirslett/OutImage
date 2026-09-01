# Compiler exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success (compile and/or run completed) |
| `1` | Compile error (lex / parse / semantic / codegen) **or** native runtime `simrt_error` |
| `2` | Usage / CLI argument error (when distinguished by the driver) |

Native binaries produced by `sim` currently exit `0` on success and `1`
on runtime Standard failures routed through `simrt_error`. Internal
runtime aborts may produce a non-zero status from the OS signal path instead.
