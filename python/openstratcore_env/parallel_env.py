"""PettingZoo ParallelEnv-compatible wrapper. Two agents: 'red', 'blue'.

backend='mock' (default): pure-Python skirmish, runnable now.
backend='rust':           the real engine via the `openstratcore_core` PyO3 extension.

The rust backend is experimental until the core mechanics land (see ROADMAP); it
demonstrates how the same training code attaches to the real engine via its
fog-of-war observation and JSON command interface.
"""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np
from gymnasium import spaces

from .mock_backend import AGENTS, N_ACTIONS, OBS_DIM, MockMatch

try:  # PettingZoo is optional; we stay API-compatible either way.
    from pettingzoo import ParallelEnv as _Base
except Exception:  # pragma: no cover
    class _Base:  # minimal stand-in
        pass


class MiaosuanParallelEnv(_Base):
    metadata = {"name": "openstratcore_commander_v0"}

    def __init__(self, backend: str = "mock", scenario_dir: str | None = None,
                 seed: int = 0, max_ticks: int = 120, opponent: str = "self",
                 scenario_file: str = "demo_skirmish.scenario.json"):
        self.backend = backend
        self.possible_agents = list(AGENTS)
        self.agents = list(AGENTS)
        self._seed = seed
        self._max_ticks = max_ticks
        self.opponent = opponent

        self._obs_space = spaces.Box(low=-1.0, high=1.0, shape=(OBS_DIM,), dtype=np.float32)
        self._act_space = spaces.Discrete(N_ACTIONS)

        if backend == "mock":
            self._impl = MockMatch(max_ticks=max_ticks)
            self.learners = list(AGENTS)  # mock is self-play only; opponent is ignored
        elif backend == "rust":
            self._impl = _RustBackend(scenario_dir, seed, max_ticks, opponent=opponent,
                                      scenario_file=scenario_file)
            # opponent="scripted" -> blue is driven by the in-repo ScriptedCommander, so only red learns
            # (no longer pure symmetric self-play). "self" -> both sides share the policy as before.
            self.learners = ["red"] if opponent == "scripted" else list(AGENTS)
        else:
            raise ValueError(f"unknown backend {backend!r}")

    # PettingZoo API ----------------------------------------------------------
    def observation_space(self, agent):
        return self._obs_space

    def action_space(self, agent):
        return self._act_space

    def reset(self, seed: int | None = None, options=None):
        self.agents = list(self.possible_agents)
        obs = self._impl.reset(self._seed if seed is None else seed)
        infos = {a: {} for a in self.agents}
        return obs, infos

    def step(self, actions: dict):
        obs, rewards, terms, truncs, infos = self._impl.step(actions)
        if all(terms.values()) or all(truncs.values()):
            self.agents = []
        return obs, rewards, terms, truncs, infos


_TICK_KEY = "decision_tick_seconds"

# RL TRAINING config (NOT game rules — these shape the learning signal, not the simulation; the mock
# backend uses the same values). Game numbers live in config/rules.*.json; these deliberately do not.
_OBS_NEAR_HEXES = 5      # coarse "enemy is close" obs feature (a hint, not the engine's real range)
_R_APPROACH = 0.05       # shaping per hex closed toward the objective
_R_TIME = 0.005          # per-step time penalty
_R_WIN = 10.0            # terminal win/lose bonus


class _RustBackend:
    """The real engine (PyO3 `openstratcore_core`) as a PettingZoo backend, drop-in for the mock:
    same 7-dim obs, same Discrete(4) action head (0 stay, 1 move -q, 2 move +q, 3 fire), shaped
    rewards, and capture/annihilation termination. The agent obs is the FOG-OF-WAR `observe()`
    projection (rule #5); reward shaping reads the god-view `snapshot()` (a training signal, not an
    agent view). Build the extension first: `make py-dev`."""

    def __init__(self, scenario_dir, seed, max_ticks, opponent: str = "self",
                 scenario_file: str = "demo_skirmish.scenario.json"):
        import openstratcore_core  # built from crates/openstratcore-py

        # Fixed scripted opponent (P1): drive blue with the in-repo ScriptedCommander so red trains
        # against a competent, deterministic teacher instead of a mirror of itself. It consumes the same
        # fog-of-war observe() JSON and already handles stop-before-fire / suppression / busyUntil.
        self._scripted_side = "blue"
        self._scripted = None
        if opponent == "scripted":
            try:
                from examples.llm_agent import ScriptedCommander
            except ImportError as e:  # examples/ is not part of the installed package (pyproject)
                raise ImportError(
                    "opponent='scripted' needs examples.llm_agent on sys.path — run from the repo's "
                    "python/ directory (e.g. PYTHONPATH=. python examples/selfplay_ppo.py ...). "
                    f"(original: {e})") from e
            self._scripted = ScriptedCommander(self._scripted_side)
        elif opponent != "self":
            raise ValueError(f"unknown opponent {opponent!r} (use 'self' or 'scripted')")

        root = Path(scenario_dir or Path(__file__).resolve().parents[2] / "scenarios")
        scenario = json.loads((root / scenario_file).read_text(encoding="utf-8"))
        map_json = (root / "maps" / scenario["map"]).read_text(encoding="utf-8")
        rules_root = Path(__file__).resolve().parents[2] / "config"
        rules_json = (rules_root / scenario.get("rules", "rules.default.json")).read_text(
            encoding="utf-8")
        rules = json.loads(rules_json)
        self._mk = lambda s: openstratcore_core.Engine(map_json, json.dumps(scenario), rules_json, s)
        self._seed = seed
        self._max_ticks = max_ticks
        self.tick_s = float(rules.get("timing", {}).get(_TICK_KEY, 5.0))
        hexes = json.loads(map_json).get("hexes", [])
        self._w = max((h["q"] for h in hexes), default=6) + 1
        objs = scenario.get("objectives", [])
        self._obj = objs[0]["at"] if objs else {"q": self._w // 2, "r": 0}
        self._obj_id = objs[0]["id"] if objs else None
        self._loadout = rules.get("loadout", {})
        # max 班/车数 per unit is rules-as-data (三.17b) — used to normalize the team obs feature.
        self._max_teams = float(rules.get("control", {}).get("max_squads_per_unit", 4))
        self.reset(seed)

    # --- helpers -------------------------------------------------------------
    @staticmethod
    def _enemy(agent: str) -> str:
        return "blue" if agent == "red" else "red"

    def _teams(self, snap: dict) -> dict:
        out = {a: 0 for a in AGENTS}
        for u in snap.get("units", {}).values():
            if u.get("alive", False):
                out[u["side"]] = out.get(u["side"], 0) + int(u.get("teams", 0))
        return out

    def _lead(self, agent: str, snap: dict):
        own = [u for u in snap.get("units", {}).values()
               if u.get("alive") and u["side"] == agent and u.get("carried_by") is None]
        own.sort(key=lambda u: u["id"])
        return own[0] if own else None

    def _hex_dist(self, a: dict, b: dict) -> float:
        dq, dr = a["q"] - b["q"], a["r"] - b["r"]
        return (abs(dq) + abs(dr) + abs(dq + dr)) / 2.0

    def _obj_dist(self, agent: str, snap: dict) -> float:
        lead = self._lead(agent, snap)
        return float(self._w) if lead is None else self._hex_dist(lead["pos"], self._obj)

    def _weapon(self, unit_type: str):
        wl = self._loadout.get(unit_type, [])
        return wl[0] if wl else None

    # --- API -----------------------------------------------------------------
    def reset(self, seed: int = 0):
        self.eng = self._mk(seed)
        # Turn on direct-fire telemetry (A2): observational, never read by the sim, so it can't affect
        # determinism — it just lets training count whether fire_direct ever actually resolves. Guarded
        # so an older wheel without the hook still trains. enable() clears the buffer → fresh per episode.
        if hasattr(self.eng, "enable_outcome_log"):
            self.eng.enable_outcome_log()
        self.t = 0
        snap = json.loads(self.eng.snapshot())
        self._prev_teams = self._teams(snap)
        self._prev_obj_dist = {a: self._obj_dist(a, snap) for a in AGENTS}
        return {a: self._obs(a) for a in AGENTS}

    def _obs(self, agent: str) -> np.ndarray:
        # Built PURELY from the fog-of-war observation (rule #5) — no god-view snapshot. Units are
        # sorted by id so the "lead unit" is a stable, deterministic choice.
        obs = json.loads(self.eng.observe(agent))
        own = sorted(obs.get("ownUnits", []), key=lambda u: u["id"])
        enemy = sorted(obs.get("enemyUnits", []), key=lambda u: u["id"])
        scale = max(self._w - 1, 1)
        own_q = (own[0]["at"]["q"] / scale) if own else 0.0
        own_teams = (own[0].get("teams", 0) / self._max_teams) if own else 0.0
        visible = bool(enemy)
        enemy_q = (enemy[0]["at"]["q"] / scale) if visible else -1.0
        obj_dist = self._hex_dist(own[0]["at"], self._obj) if own else float(self._w)
        in_range = 0.0
        if own and visible:
            in_range = 1.0 if self._hex_dist(own[0]["at"], enemy[0]["at"]) <= _OBS_NEAR_HEXES else 0.0
        # P2: firing-posture features (already in observe(), rule #5) so the policy can tell when a fire
        # order would be accepted — a moving / un-deployed / busy unit's fire is rejected (8.5c).
        lead = own[0] if own else None
        own_stopped = 1.0 if (lead and lead.get("state") == "stopped") else 0.0
        weapon_ready = 1.0 if (lead and lead.get("weaponState", "deployed") == "deployed") else 0.0
        own_busy = 1.0 if (lead and lead.get("busyUntil", 0) > 0) else 0.0
        return np.array([
            own_q,
            enemy_q,
            own_teams,
            1.0 if visible else -1.0,
            obj_dist / scale,
            in_range,
            self.t / self._max_ticks,
            own_stopped,
            weapon_ready,
            own_busy,
        ], dtype=np.float32)

    def _commands(self, agent: str, act: int, snap: dict):
        """Discrete action -> command(s) for the side's lead unit, plus an automatic 夺控 when an own
        unit stands on the objective. Every selection is id-sorted for determinism (not dict order)."""
        cmds = []
        lead = self._lead(agent, snap)  # _lead already sorts by id
        if lead is not None:
            uid, q, r = lead["id"], lead["pos"]["q"], lead["pos"]["r"]
            if act == 1:
                cmds.append({"op": "move_to", "unitId": uid, "target": {"q": q - 1, "r": r}})
            elif act == 2:
                cmds.append({"op": "move_to", "unitId": uid, "target": {"q": q + 1, "r": r}})
            elif act == 3:
                enemy = sorted(json.loads(self.eng.observe(agent)).get("enemyUnits", []),
                               key=lambda u: u["id"])
                wpn = self._weapon(lead.get("unit_type", ""))
                if enemy and wpn is not None:
                    cmds.append({"op": "fire_direct", "unitId": uid, "weapon": wpn,
                                 "targetUnit": enemy[0]["id"]})
            elif act == 4:
                # P2: 停止 to enter a firing posture — a moving ground unit must stop before it can fire
                # (8.5c). Without this action the policy could never legally fire (P0 showed ~70% of
                # orders rejected); pairing it with the new posture obs lets it learn stop→deploy→fire.
                cmds.append({"op": "stop", "unitId": uid})
        cmds.extend(self._auto_capture(agent, snap))
        return cmds

    def _auto_capture(self, agent: str, snap: dict):
        """An automatic 夺控 order when an own unit stands on the objective — an env mechanic applied to
        BOTH the learner and the scripted side (id-sorted for determinism), so neither has a capture edge."""
        on_obj = sorted(
            (u for u in snap.get("units", {}).values()
             if u.get("alive") and u["side"] == agent and u.get("carried_by") is None
             and u["pos"] == self._obj),
            key=lambda u: u["id"])
        return [{"op": "capture", "unitId": on_obj[0]["id"]}] if on_obj else []

    def step(self, actions: dict):
        snap = json.loads(self.eng.snapshot())
        # Decide ALL sides' commands from the SAME pre-submit state, THEN submit. Both sides move
        # simultaneously: neither may see the other's same-tick effects. submit() mutates visible state
        # at once (a fire/stop/capture lands immediately), so deciding the scripted side AFTER the
        # learner's submit would let it react to effects the learner never observed — an unfair, leaky
        # advantage. Freezing decisions first keeps it a true simultaneous move (and matches how the
        # learner's own obs was taken at the previous step boundary).
        planned = {}
        for agent in AGENTS:
            if self._scripted is not None and agent == self._scripted_side:
                # Scripted opponent decides from its own fog-of-war view (rule #5); still gets the env's
                # auto-capture so it can win by 夺控 too.
                obs_a = json.loads(self.eng.observe(agent))
                planned[agent] = (self._scripted.decide(obs_a).get("actions", [])
                                  + self._auto_capture(agent, snap))
            else:
                planned[agent] = self._commands(agent, int(actions.get(agent, 0)), snap)

        # Submit in canonical AGENTS order so same-tick orders get a deterministic submit sequence, and
        # tally accepted vs rejected per side: a silently-rejected order and a deliberate hold look
        # identical to the policy, so counting rejections tells us whether the standoff is no-op spam.
        accepted = {a: 0 for a in AGENTS}
        rejected = {a: 0 for a in AGENTS}
        for agent in AGENTS:
            for cmd in planned[agent]:
                try:
                    self.eng.submit(agent, json.dumps(cmd), self.t * self.tick_s)
                    accepted[agent] += 1
                except Exception:
                    rejected[agent] += 1  # invalid / mid-transition / unobserved orders are no-ops
        self.eng.step(self.tick_s)
        self.t += 1

        # Drain the direct-fire adjudications recorded during this step (A2 telemetry; observational).
        # fires = shots that reached resolution, hits = those that destroyed/annihilated. Global per step
        # (the log is not side-tagged), so it is reported identically on both agents' infos, like winner.
        fires, hits = 0, 0
        if hasattr(self.eng, "drain_outcome_log"):
            try:
                recs = json.loads(self.eng.drain_outcome_log())
                fires = len(recs)
                hits = sum(1 for r in recs
                           if str(r.get("outcome", "")).startswith(("destroyed", "kill")))
            except Exception:
                pass

        snap = json.loads(self.eng.snapshot())
        teams = self._teams(snap)
        control = snap.get("control", {})
        rewards = {a: 0.0 for a in AGENTS}
        for a in AGENTS:
            e = self._enemy(a)
            rewards[a] += float(self._prev_teams.get(e, 0) - teams.get(e, 0))  # enemy losses
            rewards[a] -= float(self._prev_teams.get(a, 0) - teams.get(a, 0))  # own losses
            d = self._obj_dist(a, snap)
            rewards[a] += _R_APPROACH * (self._prev_obj_dist[a] - d)  # approach the objective
            rewards[a] -= _R_TIME  # time penalty
            self._prev_obj_dist[a] = d
        self._prev_teams = teams

        terms = {a: False for a in AGENTS}
        winner = None
        if self._obj_id is not None and control.get(self._obj_id) in AGENTS:
            winner = control[self._obj_id]
        elif (teams.get("red", 0) == 0) != (teams.get("blue", 0) == 0):
            winner = "red" if teams.get("red", 0) > 0 else "blue"
        if winner is not None:
            rewards[winner] += _R_WIN
            rewards[self._enemy(winner)] -= _R_WIN
            terms = {a: True for a in AGENTS}

        truncs = {a: self.t >= self._max_ticks for a in AGENTS}
        obs = {a: self._obs(a) for a in AGENTS}
        infos = {a: {"winner": winner, "accepted": accepted[a], "rejected": rejected[a],
                     "fires": fires, "hits": hits} for a in AGENTS}
        return obs, rewards, terms, truncs, infos


def make_env(backend: str = "mock", **kwargs) -> MiaosuanParallelEnv:
    return MiaosuanParallelEnv(backend=backend, **kwargs)
