---
name: calibrate-prob
description: >
  Calibrate a result table's probability provider (Dirichlet-Multinomial) from replay outcomes
  and/or expert priors, switching that table to the bayesian backend. Use for "校准概率",
  "learn the hit probabilities from data".
---

# /calibrate-prob — probability learning

Operationalizes docs/WORKFLOWS.md §7 (decision Q1-P3, sources ①replay data + ②expert priors).

1. Gather outcome data to `runs/outcomes.jsonl` (records: `{table, attackLevel, outcome}`), and/or
   an expert prior JSON (`{level: {outcome: alpha}}`).
2. Run:
   `python python/prob_learning/calibrate.py <table> --data runs/outcomes.jsonl [--prior expert.json] \
       --rules config/rules.default.json --out config/rules.calibrated.json`
   This writes `prob.providers[<table>] = {kind: bayesian, params: {...}}` (posterior concentration).
3. Gate: `make validate` (calibrated rules still valid).
4. Gate: `/replay-verify` with the new rules (determinism still holds — a fixed seed reproduces).

Output: a calibrated rules config variant + posterior outcome means.
