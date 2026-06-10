---
name: replay-verify
description: >
  Verify replay determinism: re-run a recorded match from seed+commands and assert the periodic
  snapshots match tick-by-tick. Use as a CI/gate, or for "check the replay reproduces".
---

# /replay-verify — determinism regression

Operationalizes docs/WORKFLOWS.md §8. This protects CLAUDE.md rule 7 (replay must not break).

1. Take a replay JSON (header has `seed`, plus `commands` and periodic `snapshots`).
2. Build a fresh engine with `header.seed`, replay `commands` in order, and at each snapshot time
   compare the engine state to the recorded snapshot.
3. On any mismatch, report the FIRST diverging tick and the differing fields — this means
   determinism was broken (a blocking defect). Otherwise report "reproduces exactly".

Output: pass, or the first divergence. Wire into CI so every change runs it.
