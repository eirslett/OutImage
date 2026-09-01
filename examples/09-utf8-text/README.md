# 09 — UTF-8 text copy

`--charset utf8` encodes Simula ranks 0–255 as UTF-8 at the C/JS edge.
Internal texts stay one rank per character. This example puts ISO rank 233
(`é`) into a one-character text and greets C.

Expected bytes: `c3 a9 0a` (`é` plus newline).

`--charset latin1` (default) would send a single `e9` byte instead.
