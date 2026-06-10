---
name: new-scenario
description: >
  Author a new map + scenario (forces, objectives, victory, time limit, preset facilities), or
  import a map from Tiled (.tmx). Use for "做一张新地图", "create a scenario", "import this tmx".
---

# /new-scenario — author map + scenario

Operationalizes docs/WORKFLOWS.md §2.

1. **Map.** Write/edit `scenarios/maps/<name>.map.json` (AI can generate hex arrays directly),
   or import: `python tools/tmx_import.py <file>.tmx > scenarios/maps/<name>.map.json`.
2. **Scenario.** Write `scenarios/<name>.scenario.json`: sides/units/placement, objectives
   (control points), victory, time limit, preset facilities (minefields/roadblocks/works/zones).
3. **Gate:** `make validate` (both files pass their schema).
4. **Smoke:** `python tools/sim_smoke.py scenarios/<name>.scenario.json` (scripted-vs-scripted runs
   without crashing; skips if the engine isn't built yet).

Output: a matched map + scenario pair ready to load.
