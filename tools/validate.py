#!/usr/bin/env python3
"""Validate sample data against the JSON Schemas. Used by `make validate` and /new-scenario.
Requires: pip install jsonschema. Does NOT require the Rust extension."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]


def load(p: str):
    # Files are UTF-8 (rule tables carry Chinese text); never rely on the platform
    # default encoding, which is GBK/cp936 on Chinese Windows and corrupts them.
    return json.loads((ROOT / p).read_text(encoding="utf-8"))


def validate(instance_path: str, schema_path: str, subschema: str | None = None) -> bool:
    schema = load(schema_path)
    if subschema:
        schema = {"$schema": schema["$schema"], "$id": schema["$id"], **schema["$defs"][subschema],
                  "$defs": schema["$defs"]}
    inst = load(instance_path)
    errs = sorted(Draft202012Validator(schema).iter_errors(inst), key=lambda e: list(e.path))
    if errs:
        print(f"FAIL {instance_path}")
        for e in errs[:10]:
            print("   ", list(e.path), "-", e.message)
        return False
    print(f"PASS {instance_path}")
    return True


def main():
    checks = [
        ("config/rules.default.json", "schemas/rules.schema.json", None),
        # The /calibrate-prob output (a table switched static→bayesian) must stay schema-valid.
        ("config/rules.calibrated.json", "schemas/rules.schema.json", None),
        ("scenarios/maps/demo_valley.map.json", "schemas/map.schema.json", None),
        ("scenarios/demo_skirmish.scenario.json", "schemas/scenario.schema.json", None),
        ("prompts/examples/observation_example.json", "schemas/llm_tools.schema.json", "observation"),
        ("prompts/examples/tool_call_example.json", "schemas/llm_tools.schema.json", "actionList"),
    ]
    # Validate any user-authored maps/scenarios too.
    for mp in (ROOT / "scenarios" / "maps").glob("*.map.json"):
        rel = str(mp.relative_to(ROOT))
        if not any(rel == c[0] for c in checks):
            checks.append((rel, "schemas/map.schema.json", None))
    for sc in (ROOT / "scenarios").glob("*.scenario.json"):
        rel = str(sc.relative_to(ROOT))
        if not any(rel == c[0] for c in checks):
            checks.append((rel, "schemas/scenario.schema.json", None))

    ok = all(validate(*c) for c in checks)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
