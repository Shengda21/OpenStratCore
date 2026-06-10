---
name: add-rule
description: >
  Add or modify one game rule in the deterministic engine. Use when the user wants to
  implement a rule from the ruleset (e.g. "实现间瞄射击", "add cover mechanic", "/add-rule 8.3").
  Drives the full core loop: spec -> schema/config -> Rust mechanic -> deterministic test ->
  codex review -> self-play smoke -> replay-verify -> docs.
---

# /add-rule — implement one rule

This operationalizes docs/WORKFLOWS.md §1. Follow the gates in order; do not skip.

1. **Spec (Plan subagent).** Read the rule's text. Extract decidable semantics: trigger
   conditions, durations, numbers, randomness, edge cases, and interactions/exclusions with
   other rules. Produce an acceptance checklist. If anything is ambiguous, ASK — never invent.
2. **Data (if numbers/tables change).** Edit `schemas/rules.schema.json`, then add the values to
   `config/rules.default.json`. Gate: `make validate`.
3. **Locate (Explore subagent).** Find the implementation point in `crates/openstratcore-core`
   (`mechanics.rs`, `engine.rs`, etc.).
4. **Implement (rules-engineer subagent).** Code the mechanic. Randomness ONLY via
   `prob::ProbProvider` / `rng::Rng`. No panics. No hardcoded tunables.
5. **Test.** Add deterministic unit tests (fixed seed + expected outcome) for typical + edge cases.
6. **Gate:** `make test && make lint`.
7. **Gate:** run `/codex-review` and evaluate each finding (accept/reject with reason).
8. **Gate:** `make selfplay` (smoke, must not crash/diverge) and `/replay-verify`.
9. **Docs.** Update docs and tick the rule-coverage checklist in docs/WORKFLOWS.md.

Output: a single focused commit (core + tests + config + docs). On any red gate, return to the
matching step.
