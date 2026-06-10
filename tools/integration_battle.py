#!/usr/bin/env python3
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, Callable

try:
    import openstratcore_core
except ImportError:
    # The PyO3 wheel isn't built in this environment — skip gracefully (like tools/sim_smoke.py),
    # so `make verify-all` on a fresh checkout doesn't hard-fail. Build it with `make py-dev`.
    print("integration_battle: skip (openstratcore_core wheel not built — run `make py-dev`)")
    sys.exit(0)


ROOT = Path(__file__).resolve().parents[1]
DECISION_TICK_S = 5.0
SEED = 7
Q = 11
R = 4


def jdump(obj: Any) -> str:
    return json.dumps(obj, ensure_ascii=False, separators=(",", ":"))


def stable_trace(trace: list[dict[str, Any]]) -> str:
    return json.dumps(trace, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def load_rules_json() -> str:
    return (ROOT / "config" / "rules.default.json").read_text(encoding="utf-8")


def build_map_json(q_max: int = Q, r_max: int = R) -> str:
    data = {
        "format": "openstratcore.map",
        "version": 1,
        "name": "intg",
        "elevationUnitMeters": 10,
        "bounds": {"qMin": 0, "qMax": q_max, "rMin": 0, "rMax": r_max},
        "hexes": [
            {
                "q": q,
                "r": r,
                "id": f"{q:02d}{r:02d}",
                "elevation": 0,
                "terrain": "open",
            }
            for q in range(q_max + 1)
            for r in range(r_max + 1)
        ],
    }
    return jdump(data)


def unit(
    unit_id: str,
    unit_type: str,
    armor: str,
    teams: int,
    q: int,
    r: int,
    *,
    state: str = "normal",
    carried_by: str | None = None,
    affiliated_to: str | None = None,
) -> dict[str, Any]:
    data: dict[str, Any] = {
        "id": unit_id,
        "type": unit_type,
        "armor": armor,
        "teams": teams,
        "at": {"q": q, "r": r},
        "facing": 0,
        "state": state,
    }
    if carried_by is not None:
        data["carriedBy"] = carried_by
    if affiliated_to is not None:
        data["affiliatedTo"] = affiliated_to
    return data


def build_scenario_json(
    *,
    red_units: list[dict[str, Any]],
    blue_units: list[dict[str, Any]],
    facilities: list[dict[str, Any]] | None = None,
    objectives: list[dict[str, Any]] | None = None,
) -> str:
    data = {
        "format": "openstratcore.scenario",
        "version": 1,
        "name": "intg",
        "map": "intg",
        "timeLimitSeconds": 36000,
        "sides": {
            "red": {"name": "Red", "units": red_units},
            "blue": {"name": "Blue", "units": blue_units},
        },
        "objectives": objectives or [],
        "facilities": facilities or [],
    }
    return jdump(data)


def new_engine(
    *,
    red_units: list[dict[str, Any]],
    blue_units: list[dict[str, Any]],
    facilities: list[dict[str, Any]] | None = None,
    objectives: list[dict[str, Any]] | None = None,
) -> Any:
    return openstratcore_core.Engine(
        build_map_json(),
        build_scenario_json(
            red_units=red_units,
            blue_units=blue_units,
            facilities=facilities,
            objectives=objectives,
        ),
        load_rules_json(),
        SEED,
    )


def drive(
    eng: Any,
    timeline: list[tuple[float, str, dict[str, Any]]],
    total_seconds: float,
    tick_s: float = DECISION_TICK_S,
) -> tuple[
    list[dict[str, Any]],
    list[tuple[float, str, dict[str, Any]]],
    list[tuple[float, str, dict[str, Any], str]],
]:
    by_time: dict[float, list[tuple[float, str, dict[str, Any]]]] = {}
    for t, side, cmd in timeline:
        if abs((t / tick_s) - round(t / tick_s)) > 1e-9:
            raise AssertionError(f"command time is not on a decision tick: t={t}, cmd={cmd}")
        by_time.setdefault(float(t), []).append((float(t), side, cmd))

    snapshots = [json.loads(eng.snapshot())]
    accepted: list[tuple[float, str, dict[str, Any]]] = []
    rejected: list[tuple[float, str, dict[str, Any], str]] = []

    steps = int(round(total_seconds / tick_s))
    for i in range(steps):
        t = float(i) * tick_s
        for scheduled in by_time.get(t, []):
            _, side, cmd = scheduled
            try:
                eng.submit(side, jdump(cmd), t)
            except Exception as exc:
                rejected.append((t, side, cmd, str(exc)))
            else:
                accepted.append((t, side, cmd))
        eng.step(tick_s)
        snapshots.append(json.loads(eng.snapshot()))

    unsent = sorted(t for t in by_time if t > total_seconds - tick_s)
    if unsent:
        raise AssertionError(f"timeline contains commands beyond drive loop: {unsent}")

    return snapshots, accepted, rejected


def unit_in(snapshot: dict[str, Any], unit_id: str) -> dict[str, Any]:
    units = snapshot.get("units", {})
    if unit_id not in units:
        raise AssertionError(f"missing unit {unit_id}; present units={sorted(units)}")
    return units[unit_id]


def is_on_board(u: dict[str, Any]) -> bool:
    return "carried_by" not in u and "inside_facility" not in u


def pos_of(u: dict[str, Any]) -> tuple[int, int]:
    return int(u["pos"]["q"]), int(u["pos"]["r"])


def assert_accepted(
    accepted: list[tuple[float, str, dict[str, Any]]],
    rejected: list[tuple[float, str, dict[str, Any], str]],
    expected: list[tuple[float, str, dict[str, Any]]],
) -> None:
    accepted_keys = {(t, side, stable_cmd(cmd)) for t, side, cmd in accepted}
    rejected_by_key = {(t, side, stable_cmd(cmd)): err for t, side, cmd, err in rejected}

    for t, side, cmd in expected:
        key = (float(t), side, stable_cmd(cmd))
        if key in rejected_by_key:
            raise AssertionError(f"expected command accepted but rejected at t={t}: {cmd}; reason={rejected_by_key[key]}")
        if key not in accepted_keys:
            raise AssertionError(f"expected command was not accepted at t={t}: {cmd}")

    if rejected:
        details = "; ".join(f"t={t} side={side} cmd={cmd} err={err}" for t, side, cmd, err in rejected)
        raise AssertionError(f"unexpected rejected command(s): {details}")


def stable_cmd(cmd: dict[str, Any]) -> str:
    return json.dumps(cmd, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def run_twice(
    factory: Callable[[], Any],
    timeline: list[tuple[float, str, dict[str, Any]]],
    total_seconds: float,
) -> tuple[list[dict[str, Any]], list[tuple[float, str, dict[str, Any]]], list[tuple[float, str, dict[str, Any], str]]]:
    eng1 = factory()
    trace1, accepted1, rejected1 = drive(eng1, timeline, total_seconds)

    eng2 = factory()
    trace2, accepted2, rejected2 = drive(eng2, timeline, total_seconds)

    got1 = stable_trace(trace1)
    got2 = stable_trace(trace2)
    if got1 != got2:
        raise AssertionError("snapshot traces differ between same-seed replay runs")

    if stable_trace([{"accepted": accepted1, "rejected": rejected1}]) != stable_trace([{"accepted": accepted2, "rejected": rejected2}]):
        raise AssertionError("accepted/rejected command traces differ between same-seed replay runs")

    return trace1, accepted1, rejected1


def scene_aggregate_split() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[
                unit("RA", "infantry", "none", 2, 0, 0, state="stopped"),
                unit("RB", "infantry", "none", 2, 0, 0, state="stopped"),
            ],
            blue_units=[],
        )

    timeline = [
        (0.0, "red", {"op": "aggregate", "unitId": "RA", "targetUnit": "RB"}),
        (80.0, "red", {"op": "split", "unitId": "RA"}),
    ]
    trace, accepted, rejected = run_twice(factory, timeline, 200.0)
    assert_accepted(accepted, rejected, timeline)

    after_aggregate = next(s for s in trace if s["clock"] >= 7500)
    ra = unit_in(after_aggregate, "RA")
    rb = unit_in(after_aggregate, "RB")
    if ra["teams"] != 4:
        raise AssertionError(f"after aggregate expected RA.teams==4, got {ra['teams']}")
    if rb["alive"] is not False:
        raise AssertionError(f"after aggregate expected RB.alive==False, got {rb['alive']}")

    final = trace[-1]
    ra = unit_in(final, "RA")
    ra1 = unit_in(final, "RA#1")
    if ra["teams"] != 2:
        raise AssertionError(f"after split expected RA.teams==2, got {ra['teams']}")
    if not ra1["alive"] or ra1["teams"] != 2:
        raise AssertionError(f"after split expected RA#1 alive teams==2, got alive={ra1['alive']} teams={ra1['teams']}")


def scene_loitering() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[
                unit("RLV", "ifv", "light", 2, 0, 0, state="stopped"),
                unit("RLM", "loitering_munition", "none", 1, 0, 0, state="stopped", carried_by="RLV"),
            ],
            blue_units=[unit("BT", "tank", "none", 3, 6, 0, state="stopped")],
        )

    timeline = [
        (0.0, "red", {"op": "launch_loitering", "unitId": "RLM", "targetArea": {"q": 0, "r": 0}}),
        (80.0, "red", {"op": "move_to", "unitId": "RLM", "target": {"q": 4, "r": 0}}),
        (120.0, "red", {"op": "strike_loitering", "unitId": "RLM", "targetUnit": "BT"}),
    ]
    trace, accepted, rejected = run_twice(factory, timeline, 400.0)
    assert_accepted(accepted, rejected, timeline)

    final = trace[-1]
    rlm = unit_in(final, "RLM")
    if rlm["alive"] is not False:
        raise AssertionError(f"after strike expected RLM.alive==False, got {rlm['alive']}")


def scene_works_garrison() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[unit("RTK", "tank", "composite", 4, 4, 1, state="stopped")],
            blue_units=[unit("BG", "infantry", "none", 3, 6, 1, state="stopped")],
            facilities=[
                {
                    "kind": "works_infantry_combat",
                    "at": {"q": 6, "r": 1},
                    "owner": "blue",
                    "armor": "heavy",
                }
            ],
        )

    timeline = [
        (0.0, "blue", {"op": "enter_facility", "unitId": "BG", "facilityId": "FAC0"}),
        (80.0, "red", {"op": "fire_direct", "unitId": "RTK", "weapon": "大号直瞄炮", "targetFacility": "FAC0"}),
    ]
    trace, accepted, rejected = run_twice(factory, timeline, 150.0)
    assert_accepted(accepted, rejected, timeline)

    after_enter = next(s for s in trace if s["clock"] >= 7500)
    bg = unit_in(after_enter, "BG")
    if bg.get("inside_facility") != "FAC0":
        raise AssertionError(f"after enter expected BG.inside_facility=='FAC0', got {bg.get('inside_facility')!r}")


def scene_dismount() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[
                unit("RH", "transport_heli", "light", 1, 0, 0, state="stopped"),
                unit("RC", "infantry", "none", 2, 0, 0, state="stopped", carried_by="RH"),
            ],
            blue_units=[],
        )

    # 三.16e: a 运输直升机 must be at 超低空 (very_low) to 索降/装卸. The heli starts at "low", so first
    # switch down one band (low->very_low is adjacent, 75 s), then dismount (75 s).
    timeline = [
        (0.0, "red", {"op": "switch_altitude", "unitId": "RH", "altitude": "very_low"}),
        (80.0, "red", {"op": "dismount", "unitId": "RC"}),
    ]
    trace, accepted, rejected = run_twice(factory, timeline, 200.0)
    assert_accepted(accepted, rejected, timeline)

    final = trace[-1]
    rc = unit_in(final, "RC")
    rh = unit_in(final, "RH")
    if "carried_by" in rc:
        raise AssertionError(f"after dismount expected RC.carried_by absent, got {rc.get('carried_by')!r}")
    if not rc["alive"]:
        raise AssertionError("after dismount expected RC alive")
    if pos_of(rc) != pos_of(rh):
        raise AssertionError(f"after dismount expected RC.pos==RH.pos, got RC={pos_of(rc)} RH={pos_of(rh)}")


def scene_aa_fire() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[unit("RAA", "aa_missile_vehicle", "light", 2, 0, 0, state="stopped")],
            blue_units=[unit("BAH", "attack_heli", "light", 2, 2, 0, state="stopped")],
        )

    timeline = [(0.0, "red", {"op": "aa_fire", "unitId": "RAA", "targetUnit": "BAH"})]
    _trace, accepted, rejected = run_twice(factory, timeline, 30.0)
    assert_accepted(accepted, rejected, timeline)


def scene_indirect() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[unit("RAR", "artillery", "light", 2, 0, 0, state="stopped")],
            blue_units=[unit("BX", "infantry", "none", 3, 5, 0, state="stopped")],
        )

    timeline = [(0.0, "red", {"op": "plan_indirect", "unitId": "RAR", "targetHex": {"q": 5, "r": 0}})]
    _trace, accepted, rejected = run_twice(factory, timeline, 250.0)
    assert_accepted(accepted, rejected, timeline)


def scene_minelay() -> None:
    def factory() -> Any:
        return new_engine(
            red_units=[unit("RML", "minelayer", "light", 2, 0, 0, state="stopped")],
            blue_units=[],
        )

    timeline = [(0.0, "red", {"op": "lay_mines", "unitId": "RML", "targetHex": {"q": 2, "r": 0}})]
    _trace, accepted, rejected = run_twice(factory, timeline, 100.0)
    assert_accepted(accepted, rejected, timeline)


SCENES: list[tuple[str, Callable[[], None]]] = [
    ("aggregate_split", scene_aggregate_split),
    ("loitering", scene_loitering),
    ("works_garrison", scene_works_garrison),
    ("dismount", scene_dismount),
    ("aa_fire", scene_aa_fire),
    ("indirect", scene_indirect),
    ("minelay", scene_minelay),
]


def main() -> int:
    failures: list[tuple[str, str]] = []

    for name, fn in SCENES:
        try:
            fn()
        except Exception as exc:
            failures.append((name, str(exc)))
            print(f"FAIL {name}: {exc}", file=sys.stderr)
        else:
            print(f"PASS {name}")

    passed = len(SCENES) - len(failures)
    print(f"SUMMARY {passed}/{len(SCENES)} scenes passed")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())