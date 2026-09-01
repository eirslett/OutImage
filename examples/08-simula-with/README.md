# 08 — Simula `--with` (Chapter 6)

`external procedure helper = "utils"` names another Simula **module** by
file stem. `sim compile main.sim --with utils.sim` checks each file
alone, then MIR-merges. That is not concatenation (`sim run a.sim b.sim`).

The providing file **must** be named `utils.sim`.

Expected stdout: `42`
