#!/usr/bin/env python3
"""LLM commander harness. Each side is driven by a commander that receives a
fog-of-war observation (JSON) and returns an action list (JSON) conforming to
schemas/llm_tools.schema.json. Runs against the real engine (PyO3).

Commanders:
  claude       - via the Claude Code CLI (`claude -p`). Uses your Max subscription. NO API key.
  codex        - via the Codex CLI (`codex exec`). Uses your ChatGPT/Codex subscription. NO API key.
  anthropic-api- via the Anthropic REST API (anthropic SDK). Needs ANTHROPIC_API_KEY (BILLED).
  openai-api   - via the OpenAI REST API (openai SDK). Needs OPENAI_API_KEY (BILLED).
  scripted     - deterministic heuristic baseline (no model, no cost).
  human        - type JSON orders at the prompt.

    # subscription matchup (no keys): Claude vs Codex
    make py-dev
    python examples/llm_agent.py --red claude --blue codex

    # free smoke, zero external tools:
    python examples/llm_agent.py --red scripted --blue scripted

Subscription vs API:
  - claude/codex run on your subscriptions (no key). They draw on subscription usage limits, so
    they suit human-watchable / evaluation matches, not massive automated throughput.
    Gotcha: if ANTHROPIC_API_KEY is set in the env, `claude -p` bills as API instead of the
    subscription — unset it to stay on Max.
  - anthropic-api/openai-api are the billed REST paths, useful for high-throughput batch runs.

Output: a replay JSON (schemas/replay.schema.json) under runs/.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_inputs(scenario_name: str):
    # UTF-8 explicitly: the rules/map/scenario JSON carry Chinese keys, and Python on Windows would
    # otherwise default to GBK and choke.
    scen_path = ROOT / "scenarios" / f"{scenario_name}.scenario.json"
    scenario = json.loads(scen_path.read_text(encoding="utf-8"))
    map_json = (ROOT / "scenarios" / "maps" / scenario["map"]).read_text(encoding="utf-8")
    rules_json = (ROOT / "config" / scenario.get("rules", "rules.default.json")).read_text(
        encoding="utf-8")
    return scenario, map_json, rules_json


def _subschema(full: dict, name: str) -> dict:
    """Extract a named $defs subschema from schemas/llm_tools.schema.json (same trick as
    tools/validate.py) so the live observation / action-list can be validated against it."""
    return {"$schema": full["$schema"], "$id": full["$id"],
            **full["$defs"][name], "$defs": full["$defs"]}


def system_prompt() -> str:
    return (ROOT / "prompts" / "commander_system.md").read_text()


def tool_schema() -> dict:
    schema = json.loads((ROOT / "schemas" / "llm_tools.schema.json").read_text())
    return schema["$defs"]["actionList"]


def _extract_json(text: str) -> dict:
    """Pull a JSON object out of model/CLI output (tolerates code fences / prose)."""
    text = text.strip()
    if text.startswith("```"):
        text = text.strip("`")
        nl = text.find("\n")
        text = text[nl + 1:] if nl != -1 else text
    start, end = text.find("{"), text.rfind("}")
    if start != -1 and end != -1:
        return json.loads(text[start:end + 1])
    raise ValueError("no JSON object found in output")


def _commander_prompt(side: str, obs: dict) -> str:
    return (f"{system_prompt()}\n\nYou command side '{side}'.\n"
            f"Observation (JSON):\n{json.dumps(obs)}\n\n"
            'Respond with ONLY a JSON object {"actions":[...]} matching the action schema. '
            "No prose, no code fences.")


# --- Commanders --------------------------------------------------------------

# Default 直瞄 weapon by unit type, matching rules.loadout. The engine looks a weapon up in the
# attack-level tables, which use these Chinese keys (NOT the english rules.weapons keys), so the
# scripted baseline must name them the same way.
_DIRECT_WEAPON = {
    "tank": "大号直瞄炮",
    "ifv": "速射炮",
    "recon_vehicle": "速射炮",
    "ugv": "速射炮",
    "aa_gun": "速射炮",
    "infantry": "步兵轻武器",
    "minelayer": "步兵轻武器",
    "minesweeper": "步兵轻武器",
    "artillery": "步兵轻武器",
    "pickup": "步兵轻武器",
}


class ScriptedCommander:
    """Deterministic baseline: fire if an enemy is in range, else advance toward the
    nearest objective. Operates on the rich observation JSON."""

    def __init__(self, side: str):
        self.side = side

    def decide(self, obs: dict) -> dict:
        actions = []
        objs = obs.get("objectives", [])
        for u in obs.get("ownUnits", []):
            # Skip a unit that can't usefully act this tick: still mid-transition (busy — a new order
            # would be rejected "is mid-transition"), 压制 (suppressed — cannot fire/act), or carried
            # (a 被载 unit / loaded 巡飞弹 can only be dismounted/launched, not move_to'd — 三.2/三.10).
            if (u.get("busyUntil", 0) > 0 or u.get("state") == "suppressed"
                    or u.get("suppressed") or u.get("mountedIn")):
                continue
            enemy = _nearest(u["at"], obs.get("enemyUnits", []))
            weapon = _DIRECT_WEAPON.get(u.get("type"))
            in_range = enemy is not None and _dist(u["at"], enemy["at"]) <= 3
            if in_range and weapon is not None:
                if u.get("state") != "stopped":
                    # 8.5c: a ground unit must be 停止 to fire — stop first, then fire once settled.
                    actions.append({"op": "stop", "unitId": u["id"]})
                elif u.get("weaponState", "deployed") == "deployed":
                    actions.append({"op": "fire_direct", "unitId": u["id"],
                                    "weapon": weapon, "targetUnit": enemy["id"]})
                # else: stopped but weapon 锁定/冷却中 — hold this tick (no spam), fire when ready.
            elif objs:
                tgt = min(objs, key=lambda o: _dist(u["at"], o["at"]))["at"]
                actions.append({"op": "move_to", "unitId": u["id"], "target": tgt})
        return {"actions": actions or [{"op": "wait"}]}


class ClaudeCliCommander:
    """Drives Claude via the Claude Code CLI in headless print mode (`claude -p`).
    Uses your Max subscription (OAuth / CLAUDE_CODE_OAUTH_TOKEN) — no API key, no per-token bill.
    Flags may vary by CLI version; falls back to scripted on any error."""

    def __init__(self, side: str):
        self.side = side
        self._fallback = ScriptedCommander(side)

    def decide(self, obs: dict) -> dict:
        try:
            out = subprocess.run(
                ["claude", "-p", _commander_prompt(self.side, obs), "--output-format", "json"],
                capture_output=True, text=True, timeout=120, check=True,
            )
            try:
                env = json.loads(out.stdout)
                text = env.get("result", out.stdout) if isinstance(env, dict) else out.stdout
            except json.JSONDecodeError:
                text = out.stdout
            return _extract_json(text)
        except Exception as e:
            print(f"[claude-cli] fallback to scripted: {e}")
            return self._fallback.decide(obs)


class CodexCliCommander:
    """Drives a model via the Codex CLI (`codex exec`). Uses your ChatGPT/Codex subscription —
    no API key. Constrains output with --output-schema and reads the last message via -o."""

    def __init__(self, side: str):
        self.side = side
        self.schema = tool_schema()
        self._fallback = ScriptedCommander(side)

    def decide(self, obs: dict) -> dict:
        try:
            with tempfile.TemporaryDirectory() as d:
                schema_path = os.path.join(d, "schema.json")
                out_path = os.path.join(d, "out.json")
                Path(schema_path).write_text(json.dumps(self.schema))
                subprocess.run(
                    ["codex", "exec", "-s", "read-only",
                     "--output-schema", schema_path, "-o", out_path,
                     _commander_prompt(self.side, obs)],
                    capture_output=True, text=True, timeout=180, check=True,
                )
                return _extract_json(Path(out_path).read_text())
        except Exception as e:
            print(f"[codex-cli] fallback to scripted: {e}")
            return self._fallback.decide(obs)


class AnthropicApiCommander:
    """OPTIONAL billed path: Anthropic REST API via the SDK. Needs ANTHROPIC_API_KEY.
    Useful for high-throughput batch matches; for normal play prefer the `claude` CLI commander."""

    def __init__(self, side: str):
        import anthropic
        self.side = side
        self.client = anthropic.Anthropic()
        self.model = os.environ.get("MIAOSUAN_ANTHROPIC_MODEL", "claude-opus-4-8")
        self.tool = {"name": "submit_orders",
                     "description": "Submit this side's orders for the current decision tick.",
                     "input_schema": tool_schema()}
        self._fallback = ScriptedCommander(side)

    def decide(self, obs: dict) -> dict:
        try:
            msg = self.client.messages.create(
                model=self.model, max_tokens=1024, system=system_prompt(),
                tools=[self.tool], tool_choice={"type": "tool", "name": "submit_orders"},
                messages=[{"role": "user", "content": f"You command side '{self.side}'.\n"
                                                       f"Observation:\n{json.dumps(obs)}"}],
            )
            for block in msg.content:
                if getattr(block, "type", None) == "tool_use":
                    return block.input
        except Exception as e:
            print(f"[anthropic-api] fallback to scripted: {e}")
        return self._fallback.decide(obs)


class OpenAiApiCommander:
    """OPTIONAL billed path: OpenAI REST API via the SDK. Needs OPENAI_API_KEY.
    For normal play prefer the `codex` CLI commander (subscription)."""

    def __init__(self, side: str):
        from openai import OpenAI
        self.side = side
        self.client = OpenAI()
        self.model = os.environ.get("MIAOSUAN_OPENAI_MODEL", "gpt-5.5")
        self.tools = [{"type": "function", "function": {
            "name": "submit_orders",
            "description": "Submit this side's orders for the current decision tick.",
            "parameters": tool_schema()}}]
        self._fallback = ScriptedCommander(side)

    def decide(self, obs: dict) -> dict:
        try:
            resp = self.client.chat.completions.create(
                model=self.model, tools=self.tools,
                tool_choice={"type": "function", "function": {"name": "submit_orders"}},
                messages=[{"role": "system", "content": system_prompt()},
                          {"role": "user", "content": f"You command side '{self.side}'.\n"
                                                       f"Observation:\n{json.dumps(obs)}"}],
            )
            call = resp.choices[0].message.tool_calls[0]
            return json.loads(call.function.arguments)
        except Exception as e:
            print(f"[openai-api] fallback to scripted: {e}")
        return self._fallback.decide(obs)


class HumanCommander:
    def __init__(self, side: str):
        self.side = side

    def decide(self, obs: dict) -> dict:
        print(json.dumps(obs, indent=2))
        raw = input(f"[{self.side}] actions JSON (enter = wait): ").strip()
        if not raw:
            return {"actions": [{"op": "wait"}]}
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return {"actions": [{"op": "wait"}]}


COMMANDERS = {
    "claude": ClaudeCliCommander,
    "codex": CodexCliCommander,
    "anthropic-api": AnthropicApiCommander,
    "openai-api": OpenAiApiCommander,
    "scripted": ScriptedCommander,
    "human": HumanCommander,
}


def make_commander(kind: str, side: str):
    return COMMANDERS[kind](side)


# --- helpers -----------------------------------------------------------------

def _dist(a, b):
    aq, ar, bq, br = a["q"], a["r"], b["q"], b["r"]
    return (abs(aq - bq) + abs(ar - br) + abs((-aq - ar) - (-bq - br))) // 2


def _nearest(at, units):
    return min(units, key=lambda u: _dist(at, u["at"])) if units else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--red", default="claude", choices=list(COMMANDERS))
    ap.add_argument("--blue", default="codex", choices=list(COMMANDERS))
    ap.add_argument("--scenario", default="demo_skirmish")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--ticks", type=int, default=60)
    ap.add_argument("--out", default="runs")
    ap.add_argument("--dry-run", action="store_true",
                    help="short smoke: validate every live fog-of-war observation and action-list "
                         "against schemas/llm_tools.schema.json; run the engine; write no replay. "
                         "Exits non-zero on any schema violation (LLM-interface / 迷雾观测 alignment).")
    args = ap.parse_args()

    import openstratcore_core  # built via maturin (make py-dev)

    scenario, map_json, rules_json = load_inputs(args.scenario)
    eng = openstratcore_core.Engine(map_json, json.dumps(scenario), rules_json, args.seed)
    # The decision step is rules-as-data — read it from the rules so sim_t / eng.step / the engine's
    # observed `tick` all agree even for a non-5s ruleset.
    tick_s = float(json.loads(rules_json).get("timing", {}).get("decision_tick_seconds", 5.0))

    validators = None
    dry_stats = {"obs_ok": 0, "obs_fail": 0, "act_ok": 0, "act_fail": 0, "rejects": 0}
    if args.dry_run:
        from jsonschema import Draft202012Validator
        tools = json.loads((ROOT / "schemas" / "llm_tools.schema.json").read_text())
        validators = {"obs": Draft202012Validator(_subschema(tools, "observation")),
                      "acts": Draft202012Validator(_subschema(tools, "actionList"))}
        args.ticks = min(args.ticks, 10)

    commanders = {"red": make_commander(args.red, "red"),
                  "blue": make_commander(args.blue, "blue")}

    header = {
        "format": "openstratcore.replay", "version": 1,
        "redName": scenario["sides"]["red"]["name"],
        "blueName": scenario["sides"]["blue"]["name"],
        "mapFile": scenario["map"],
        "scenarioFile": f"{args.scenario}.scenario.json",
        "rulesFile": scenario.get("rules", "rules.default.json"),
        "seed": args.seed,
        "createdAt": dt.datetime.now(dt.timezone.utc).isoformat(),
    }
    commands = []
    # Periodic full-state snapshots so the ReplayViewer can render the real 态势 at any scrubbed time
    # (schemas/replay.schema.json `snapshots`). One per tick boundary, starting from the initial state.
    snapshots = [{"t": 0.0, "state": json.loads(eng.snapshot())}]

    for t in range(args.ticks):
        sim_t = t * tick_s
        for side in ("red", "blue"):
            obs = json.loads(eng.observe(side))  # the engine now emits `tick` itself (schema-conform)
            if validators is not None:
                errs = sorted(validators["obs"].iter_errors(obs), key=lambda e: list(e.path))
                if errs:
                    dry_stats["obs_fail"] += 1
                    print(f"[dry-run] observation schema violation ({side} t{t}): {errs[0].message}")
                else:
                    dry_stats["obs_ok"] += 1
            decision = commanders[side].decide(obs)
            if validators is not None:
                errs = sorted(validators["acts"].iter_errors(decision), key=lambda e: list(e.path))
                if errs:
                    dry_stats["act_fail"] += 1
                    print(f"[dry-run] action-list schema violation ({side} t{t}): {errs[0].message}")
                else:
                    dry_stats["act_ok"] += 1
            for action in decision.get("actions", []):
                commands.append({"t": sim_t, "side": side, "command": action})
                try:
                    eng.submit(side, json.dumps(action), sim_t)
                except Exception as e:
                    dry_stats["rejects"] += 1
                    print(f"[engine] {side} {action.get('op')} -> {e}")
        eng.step(tick_s)
        snapshots.append({"t": (t + 1) * tick_s, "state": json.loads(eng.snapshot())})

    if args.dry_run:
        ok = dry_stats["obs_fail"] == 0 and dry_stats["act_fail"] == 0
        print(f"dry-run {'OK' if ok else 'FAIL'}: "
              f"{dry_stats['obs_ok']} observations + {dry_stats['act_ok']} action-lists schema-valid; "
              f"{dry_stats['obs_fail'] + dry_stats['act_fail']} schema violations; "
              f"{dry_stats['rejects']} engine rejections (expected for some scripted orders)")
        raise SystemExit(0 if ok else 1)

    header["durationSeconds"] = args.ticks * tick_s
    replay = {"header": header, "commands": commands, "snapshots": snapshots}

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    path = out / f"{args.scenario}.replay.json"
    path.write_text(json.dumps(replay, indent=2))
    print(f"wrote replay -> {path}  ({len(commands)} commands)")


if __name__ == "__main__":
    main()
