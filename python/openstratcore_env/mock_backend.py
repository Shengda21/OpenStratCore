"""A tiny, deterministic hex-skirmish used to exercise the RL loop before the Rust
engine is complete. Intentionally simple but a real learning task: two units on a
short hex line, an objective in the middle, move + fire, fog-of-war by range.

This is NOT the game. It is a stand-in with the same env API so training code is
ready to switch to backend="rust" once the core mechanics land.
"""
from __future__ import annotations

import numpy as np

AGENTS = ["red", "blue"]
OBS_DIM = 10
N_ACTIONS = 5  # 0 stay, 1 move -q, 2 move +q, 3 fire, 4 stop (enter firing posture; no-op in the mock)


class MockMatch:
    def __init__(self, width: int = 7, max_ticks: int = 120, fire_range: int = 2):
        self.W = width
        self.obj = width // 2
        self.max_ticks = max_ticks
        self.fire_range = fire_range
        self._rng = np.random.default_rng(0)
        self.reset(0)

    def reset(self, seed: int = 0):
        self._rng = np.random.default_rng(seed)
        self.t = 0
        self.pos = {"red": 0, "blue": self.W - 1}
        self.hp = {"red": 3, "blue": 3}
        self.capture = {"red": 0, "blue": 0}
        self.done = False
        return {a: self._obs(a) for a in AGENTS}

    def _enemy(self, a: str) -> str:
        return "blue" if a == "red" else "red"

    def _visible(self, a: str) -> bool:
        e = self._enemy(a)
        return abs(self.pos[a] - self.pos[e]) <= self.fire_range + 1

    def _obs(self, a: str) -> np.ndarray:
        e = self._enemy(a)
        enemy_q = self.pos[e] / (self.W - 1) if self._visible(a) else -1.0
        enemy_hp = self.hp[e] / 3.0 if self._visible(a) else -1.0
        in_range = 1.0 if abs(self.pos[a] - self.pos[e]) <= self.fire_range else 0.0
        return np.array(
            [
                self.pos[a] / (self.W - 1),
                enemy_q,
                self.hp[a] / 3.0,
                enemy_hp,
                abs(self.pos[a] - self.obj) / (self.W - 1),
                in_range,
                self.t / self.max_ticks,
                # Posture features (parity with the rust backend): the mock has no stop-before-fire
                # constraint, so a unit is always "stopped, weapon ready, not busy".
                1.0,
                1.0,
                0.0,
            ],
            dtype=np.float32,
        )

    def step(self, actions: dict):
        rewards = {a: 0.0 for a in AGENTS}
        prev_obj_dist = {a: abs(self.pos[a] - self.obj) for a in AGENTS}

        # Move phase (simultaneous).
        for a in AGENTS:
            act = int(actions[a])
            if act == 1:
                self.pos[a] = max(0, self.pos[a] - 1)
            elif act == 2:
                self.pos[a] = min(self.W - 1, self.pos[a] + 1)

        # Fire phase (simultaneous; first-come not needed at this scale).
        fires, hits = 0, 0  # diagnostics parity with the rust backend's outcome-log counts
        for a in AGENTS:
            if int(actions[a]) == 3:
                e = self._enemy(a)
                if abs(self.pos[a] - self.pos[e]) <= self.fire_range:
                    fires += 1
                    # hit probability falls with distance
                    d = abs(self.pos[a] - self.pos[e])
                    p_hit = 0.8 - 0.2 * d
                    if self._rng.random() < max(0.1, p_hit):
                        hits += 1
                        self.hp[e] -= 1
                        rewards[a] += 1.0
                        rewards[e] -= 1.0

        # Capture progress: on objective and enemy not adjacent.
        for a in AGENTS:
            e = self._enemy(a)
            if self.pos[a] == self.obj and abs(self.pos[e] - self.obj) > 1:
                self.capture[a] += 1
            else:
                self.capture[a] = 0

        # Shaping: reward getting closer to objective.
        for a in AGENTS:
            rewards[a] += 0.05 * (prev_obj_dist[a] - abs(self.pos[a] - self.obj))
            rewards[a] -= 0.005  # small time penalty

        self.t += 1
        terminations = {a: False for a in AGENTS}
        truncations = {a: False for a in AGENTS}

        winner = None
        for a in AGENTS:
            if self.hp[self._enemy(a)] <= 0:
                winner = a
            if self.capture[a] >= 6:
                winner = a
        if winner is not None:
            rewards[winner] += 10.0
            rewards[self._enemy(winner)] -= 10.0
            terminations = {a: True for a in AGENTS}
            self.done = True
        elif self.t >= self.max_ticks:
            truncations = {a: True for a in AGENTS}
            self.done = True

        obs = {a: self._obs(a) for a in AGENTS}
        infos = {a: {"winner": winner, "fires": fires, "hits": hits} for a in AGENTS}
        return obs, rewards, terminations, truncations, infos
