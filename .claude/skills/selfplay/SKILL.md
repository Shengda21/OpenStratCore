---
name: selfplay
description: >
  Run RL self-play training or a smoke regression. Use for "训练一下", "run self-play",
  "does the RL loop still work", or as a gate inside /add-rule.
---

# /selfplay — RL self-play (rl-engineer subagent)

Operationalizes docs/WORKFLOWS.md §5.

1. Ensure the env API matches the engine (mock backend needs no build).
2. Run `python examples/selfplay_ppo.py --backend <mock|rust> --total-steps <N>` (logs to `runs/`).
   - Smoke (default gate): `--backend mock --total-steps 20000`; verify it runs, return doesn't
     diverge to NaN, and it exits cleanly.
3. To upgrade the algorithm, edit `examples/selfplay_ppo.py` or add `examples/<algo>.py`; the env
   interface (two agents, reset/step) stays fixed.

Output: training curve/checkpoint, or a "smoke passed" note.
