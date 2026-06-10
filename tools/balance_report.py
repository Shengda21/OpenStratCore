#!/usr/bin/env python3
"""Balance telemetry + report (A2 — analysis only, combat model unchanged).

Runs scripted-vs-scripted matches across the scenario library with the engine's direct-fire
OUTCOME TELEMETRY enabled (enable_outcome_log / drain_outcome_log — observational, off by default),
collects every adjudication {table, weapon, attackLevel, armor, distance, outcome} from the REAL
combat pipeline, writes them to runs/outcomes.jsonl, and prints a balance summary (outcome
distribution by table×armor, and average loss per shot by weapon). This is for tuning insight; it
does NOT feed back into the simulation.

Skips gracefully if the wheel is not built. Usage: python tools/balance_report.py
"""
import collections
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

try:
    import openstratcore_core as osc
    from examples.llm_agent import ScriptedCommander
except Exception as e:  # wheel not built / import error
    print(f"balance_report: skip ({e})")
    sys.exit(0)

SCENARIOS = ["demo_skirmish", "ridge_assault", "air_assault", "combined_arms"]
SIDES = ("red", "blue")


def run_match(scenario: str, seed: int = 7, ticks: int = 160):
    scd = json.loads((ROOT / "scenarios" / f"{scenario}.scenario.json").read_text(encoding="utf-8"))
    map_json = (ROOT / "scenarios" / "maps" / scd["map"]).read_text(encoding="utf-8")
    rules_json = (ROOT / "config" / scd.get("rules", "rules.default.json")).read_text(encoding="utf-8")
    # decision_tick_seconds is a required rules tunable (the engine reads it as required at construction),
    # so take it from config rather than hardcoding a default here (规则即数据 #2).
    tick_s = float(json.loads(rules_json)["timing"]["decision_tick_seconds"])
    eng = osc.Engine(map_json, json.dumps(scd), rules_json, seed)
    eng.enable_outcome_log()
    cmd = {s: ScriptedCommander(s) for s in SIDES}
    recs = []
    for t in range(ticks):
        for side in SIDES:
            obs = json.loads(eng.observe(side))
            for a in cmd[side].decide(obs).get("actions", []):
                try:
                    eng.submit(side, json.dumps(a), t * tick_s)
                except Exception:
                    pass
        eng.step(tick_s)
        recs.extend(json.loads(eng.drain_outcome_log()))
    return recs


def _cat(outcome: str) -> str:
    if outcome.startswith("destroyed"):
        return "destroyed"
    return outcome  # suppress / noeffect / kill


def main():
    all_recs = []
    for s in SCENARIOS:
        r = run_match(s)
        all_recs.extend(r)
        print(f"  {s:16s}: {len(r)} adjudications")
    out = ROOT / "runs" / "outcomes.jsonl"
    out.parent.mkdir(exist_ok=True)
    with out.open("w", encoding="utf-8") as f:
        for r in all_recs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(all_recs)} records -> runs/outcomes.jsonl")

    if not all_recs:
        print("\n(no adjudications recorded — scripted bots never engaged)")
        return

    print("\n=== outcome distribution by (table, armor) ===")
    by_key = collections.defaultdict(collections.Counter)
    for r in all_recs:
        by_key[(r["table"], r["armor"])][_cat(r["outcome"])] += 1
    for key in sorted(by_key):
        c = by_key[key]
        n = sum(c.values())
        dist = "  ".join(f"{k}={v / n:.0%}" for k, v in sorted(c.items()))
        print(f"  {key[0]:20s} armor={key[1]:9s} n={n:4d}   {dist}")

    print("\n=== average loss (vehicles/班 destroyed) per shot, by weapon ===")
    by_weapon = collections.defaultdict(lambda: [0, 0])
    for r in all_recs:
        n = int(r["outcome"].split(":")[1]) if r["outcome"].startswith("destroyed") else 0
        by_weapon[r["weapon"]][0] += n
        by_weapon[r["weapon"]][1] += 1
    for w in sorted(by_weapon):
        tot, cnt = by_weapon[w]
        print(f"  {w:18s} n={cnt:4d}  avg_loss={tot / cnt:.2f}")


if __name__ == "__main__":
    main()
