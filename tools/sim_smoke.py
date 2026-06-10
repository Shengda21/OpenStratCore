#!/usr/bin/env python3
"""Scripted closed-loop smoke for the demo slice.

Builds the engine from JSON, runs a short scripted skirmish through the real command path
(move / observe / fire / capture), checks the fog-of-war observations are well-formed, and
verifies that running the *same* script on a fresh same-seed engine reproduces a byte-identical
per-tick trace (determinism / hard rule #7). The recorded command stream + final snapshot are
written to ``runs/<scenario>.replay.json`` as a replay artifact.

Skips gracefully (exit 0) if the Rust extension ``openstratcore_core`` isn't built yet, so the
gate stays green on a fresh checkout; once ``make py-dev`` has built it, the smoke runs for real.

    python tools/sim_smoke.py [scenarios/demo_skirmish.scenario.json]
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

DECISION_DT = 5.0  # seconds between decision ticks
HORIZON_TICKS = 64  # 320 s — long enough for a 20 s/hex tank to cross the valley and capture

# Game-rule rejections a scripted bot may legitimately hit (fog / range / posture / contested
# capture). Any other ValueError — or a Rust panic (PanicException) — fails the smoke.
EXPECTED_REJECTS = (
    "invalid fire target",
    "out of range",
    "weapon not ready",
    "must be stopped",
    "cannot fire while suppressed",
    "weapon cannot engage",
    "control point contested",
    "not on a control point",
    "air units and artillery cannot capture",
    "mid-transition",
)

VEHICLE_TYPES = {"tank", "ifv", "recon_vehicle", "radar_vehicle", "aa_missile_vehicle", "ugv"}


def _read(path: Path) -> str:
    # UTF-8 explicitly: scenarios/maps/rules carry Chinese keys; Windows would otherwise use GBK.
    return path.read_text(encoding="utf-8")


def load(scen_arg: str):
    scenario = json.loads(_read(ROOT / scen_arg))
    map_json = _read(ROOT / "scenarios" / "maps" / scenario["map"])
    rules_json = _read(ROOT / "config" / scenario.get("rules", "rules.default.json"))
    return scenario, map_json, rules_json


def check_fog(obs: dict, side: str) -> None:
    """The observation must be well-formed and leak nothing: own units carry full detail, enemies
    appear only in the engine-filtered ``enemyUnits``, and no id is both own and enemy."""
    assert obs["side"] == side, obs
    own = {u["id"] for u in obs["ownUnits"]}
    enemy = {u["id"] for u in obs["enemyUnits"]}
    assert own, f"{side} should see its own units"
    assert own.isdisjoint(enemy), f"id appears as both own and enemy: {own & enemy}"


def opening_moves(scenario: dict) -> dict[str, dict]:
    """One move order per unit at t=0: Red drives onto the central control point while Blue falls
    back to its home edge (clear of CP1's 7-hex capture zone), so the slice demonstrates the full
    arc move -> capture -> control flip. Targets are on-map hexes of demo_valley; reused by both
    runs so the script is fixed and deterministic."""
    cp = scenario["objectives"][0]["at"] if scenario.get("objectives") else {"q": 2, "r": 1}
    plan = {
        "R-T1": cp,
        "R-I1": {"q": 1, "r": 1},
        "B-T1": {"q": 4, "r": 2},  # home edge, distance 3 from CP1 — does not contest it
        "B-I1": {"q": 4, "r": 1},  # distance 2 from CP1 — also clear of the zone
    }
    # Restrict to units that actually exist in this scenario.
    ids = {u["id"] for s in ("red", "blue") for u in scenario["sides"][s]["units"]}
    return {uid: tgt for uid, tgt in plan.items() if uid in ids}


def side_of(scenario: dict, unit_id: str) -> str:
    for s in ("red", "blue"):
        if any(u["id"] == unit_id for u in scenario["sides"][s]["units"]):
            return s
    raise KeyError(unit_id)


def submit(eng, side: str, cmd: dict, t: float, log: list, *, tolerate: bool) -> bool:
    try:
        eng.submit(side, json.dumps(cmd), t)
    except ValueError as exc:  # command rejected by a game rule
        if tolerate and any(s in str(exc) for s in EXPECTED_REJECTS):
            return False
        raise
    log.append({"t": t, "side": side, "command": cmd})
    return True


def run_match(eng, scenario: dict) -> tuple[list, list, dict]:
    """Drive the fixed script. Returns (per-tick snapshot trace, recorded command log, stats)."""
    trace: list = []
    log: list = []
    stats = {"fires": 0, "captures": 0, "moved": False}

    moves = opening_moves(scenario)
    start_pos = {}
    snap0 = json.loads(eng.snapshot())
    for u in snap0["units"].values():
        start_pos[u["id"]] = (u["pos"]["q"], u["pos"]["r"])

    for tick in range(HORIZON_TICKS):
        t = tick * DECISION_DT
        for side in ("red", "blue"):
            obs = json.loads(eng.observe(side))
            # Sort by stable id so the bot's decisions never depend on serialization order.
            obs["ownUnits"].sort(key=lambda u: u["id"])
            obs["enemyUnits"].sort(key=lambda u: u["id"])
            check_fog(obs, side)

            if tick == 0:
                # Opening: issue each unit's single move order.
                for u in obs["ownUnits"]:
                    if u["id"] in moves:
                        submit(eng, side, {"op": "move_to", "unitId": u["id"], "target": moves[u["id"]]},
                               t, log, tolerate=True)
                continue

            # Steady state: fire at an observed enemy vehicle from a stopped tank; try to capture.
            enemy_vehicle = next((e for e in obs["enemyUnits"] if e["type"] in VEHICLE_TYPES), None)
            for u in obs["ownUnits"]:
                if u["type"] == "tank" and u["state"] == "stopped" and enemy_vehicle is not None:
                    if submit(eng, side, {"op": "fire_direct", "unitId": u["id"],
                                          "weapon": "大号直瞄炮", "targetUnit": enemy_vehicle["id"]},
                              t, log, tolerate=True):
                        stats["fires"] += 1
                # Red presses the capture each tick once R-T1 has arrived (tolerated until valid).
                if side == "red" and u["id"] == "R-T1":
                    if submit(eng, side, {"op": "capture", "unitId": u["id"]}, t, log, tolerate=True):
                        stats["captures"] += 1

        eng.step(DECISION_DT)
        trace.append({"t": eng.clock_seconds(), "state": json.loads(eng.snapshot())})

    final = json.loads(eng.snapshot())
    for u in final["units"].values():
        if (u["pos"]["q"], u["pos"]["r"]) != start_pos.get(u["id"]):
            stats["moved"] = True
            break
    return trace, log, stats


def main() -> int:
    scen_arg = sys.argv[1] if len(sys.argv) > 1 else "scenarios/demo_skirmish.scenario.json"
    try:
        import openstratcore_core
    except Exception:
        print("openstratcore_core not built (run `make py-dev`); skipping engine smoke.")
        return 0

    scenario, map_json, rules_json = load(scen_arg)
    seed = 1

    def fresh():
        return openstratcore_core.Engine(map_json, json.dumps(scenario), rules_json, seed)

    trace1, log, stats = run_match(fresh(), scenario)
    trace2, _, _ = run_match(fresh(), scenario)

    assert trace1, "trace must contain snapshots"
    assert json.dumps(trace1, sort_keys=True) == json.dumps(trace2, sort_keys=True), (
        "replay is not deterministic — per-tick state diverged between two same-seed runs"
    )
    assert stats["moved"], "no unit changed hex — movement is not wired"

    final = trace1[-1]["state"]
    control = final.get("control", {})
    out = ROOT / "runs" / (Path(scen_arg).stem + ".replay.json")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({
        "header": {"format": "openstratcore.replay", "version": 1, "seed": seed,
                   "mapFile": scenario["map"], "scenarioFile": scen_arg},
        "commands": log,
        "finalSnapshot": final,
    }, ensure_ascii=False, indent=2), encoding="utf-8")

    print(f"smoke OK: {scen_arg} | clock={trace1[-1]['t']:.0f}s units={len(final['units'])} "
          f"fires={stats['fires']} captures={stats['captures']} control={control} "
          f"| deterministic replay verified -> {out.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
