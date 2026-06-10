---
name: llm-match
description: >
  Run a commander match (LLM vs LLM, LLM vs scripted, or human) and record a replay. Use for
  "让 Claude 打一局", "run an llm match", "play vs the scripted bot".
---

# /llm-match — LLM commander match

Operationalizes docs/WORKFLOWS.md §6.

1. Pick commanders: `--red`/`--blue` in `{claude, codex, anthropic-api, openai-api, scripted, human}`.
   - `claude` (Claude Code CLI) and `codex` (Codex CLI) use your Max / ChatGPT **subscriptions** — no API key.
   - `anthropic-api` / `openai-api` are the optional **billed** REST paths (need API keys).
2. Run `python examples/llm_agent.py --red <r> --blue <b> --scenario <name>` (requires the built
   engine; `make py-dev`). Each decision tick builds a fog-of-war observation, calls the commander
   via the `submit_orders` tool (schemas/llm_tools.schema.json), and applies the actions.
3. A replay JSON is written to `runs/` with a full header (names/map/scenario/seed/...).

Cost: `claude`/`codex` run on your subscriptions (no key; they draw on subscription usage limits, so
best for human-watchable / evaluation matches). Use `anthropic-api`/`openai-api` only for billed
high-throughput batches. Gotcha: if `ANTHROPIC_API_KEY` is set, `claude -p` bills as API — unset it
to stay on Max. Free zero-tool smoke: `--red scripted --blue scripted`.

Output: one match + `runs/<name>.replay.json`.
