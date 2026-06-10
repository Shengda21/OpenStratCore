"""openstratcore_env — RL/LLM environment wrappers around the openstratcore engine.

Two backends:
  - "mock": a tiny pure-Python hex skirmish. Zero build, deterministic, runnable now.
  - "rust": the real engine via the PyO3 extension `openstratcore_core` (build with maturin).

The env exposes a PettingZoo ParallelEnv-compatible API (two agents: "red", "blue").
"""

from .parallel_env import MiaosuanParallelEnv, make_env

__all__ = ["MiaosuanParallelEnv", "make_env"]
