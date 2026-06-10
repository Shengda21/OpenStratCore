#!/usr/bin/env python3
"""资源闭环冒烟 (RESOURCES.md): a resource (map / scenario / rules) — whether the built-in baseline or
a user resource exported by the editors — must (1) validate against its schema (硬规则#3) and (2) load
into the single Rust engine and run. This proves there is no "editor one way, engine another" drift.

Default run uses the built-in demo resources. Pass --scenario/--map/--rules to point at exported
user resources (e.g. ScenarioEditor's user_scenario.json) and verify they round-trip into the engine.

    python tools/resource_smoke.py
    python tools/resource_smoke.py --scenario runs/user_scenario.json
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def _load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def _validate(kind: str, data: dict, subschema: str | None = None) -> list[str]:
    from jsonschema import Draft202012Validator

    schema = _load(ROOT / "schemas" / f"{kind}.schema.json")
    if subschema:
        schema = {"$schema": schema["$schema"], "$id": schema["$id"],
                  **schema["$defs"][subschema], "$defs": schema["$defs"]}
    return [f"{e.instancePath or '(root)'} {e.message}"
            for e in Draft202012Validator(schema).iter_errors(data)]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scenario", default=str(ROOT / "scenarios" / "demo_skirmish.scenario.json"))
    ap.add_argument("--map", default=None, help="defaults to the scenario's referenced map")
    ap.add_argument("--rules", default=None, help="defaults to the scenario's referenced rules")
    ap.add_argument("--ticks", type=int, default=20)
    args = ap.parse_args()

    scenario = _load(Path(args.scenario))
    map_path = Path(args.map) if args.map else ROOT / "scenarios" / "maps" / scenario["map"]
    rules_path = (Path(args.rules) if args.rules
                  else ROOT / "config" / scenario.get("rules", "rules.default.json"))
    game_map = _load(map_path)
    rules = _load(rules_path)

    # (1) every resource is schema-valid (the contract the editors export against).
    problems = {
        "scenario": _validate("scenario", scenario),
        "map": _validate("map", game_map),
        "rules": _validate("rules", rules),
    }
    for kind, errs in problems.items():
        if errs:
            print(f"resource-smoke FAIL: {kind} invalid: {errs[0]}")
            return 1

    # (2) the engine consumes them and runs (PyO3). Requires `make py-dev`.
    try:
        import openstratcore_core
    except ImportError:
        print("resource-smoke SKIP: openstratcore_core not built (make py-dev) — schema check passed")
        return 0
    tick_s = float(rules.get("timing", {}).get("decision_tick_seconds", 5.0))
    eng = openstratcore_core.Engine(map_path.read_text(encoding="utf-8"),
                                    json.dumps(scenario), rules_path.read_text(encoding="utf-8"), 1)
    for _ in range(args.ticks):
        eng.step(tick_s)
    own = len(json.loads(eng.observe("red")).get("ownUnits", []))
    print(f"resource-smoke OK: {Path(args.scenario).name} schema-valid + engine ran "
          f"{args.ticks} ticks (red sees {own} own units)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
