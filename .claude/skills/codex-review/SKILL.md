---
name: codex-review
description: >
  Get a non-interactive second-opinion review of the working diff from Codex, prioritizing
  determinism, fog-of-war, contracts, and no-panic. Use after implementing a change, or for
  "review my diff", "codex review".
---

# /codex-review — Codex diff review (reviewer subagent)

Operationalizes docs/WORKFLOWS.md §4.

1. Run `bash tools/codex_review.sh <base>` (default base `main`; read-only sandbox).
2. Parse Codex's findings. For EACH: decide accept or reject and state why — you own the call,
   Codex is advisory.
3. For accepted findings, return to the originating workflow (usually /add-rule) to fix, then
   re-review if substantial.

Output: a short disposition log (finding -> accepted/rejected -> action).
