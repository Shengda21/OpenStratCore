**English** | [简体中文](README.zh-CN.md)

# OpenStratCore

An open-source, AI-friendly, modular **real-time** hex-grid land-warfare wargame platform. Built for
next-generation wargaming: it plugs into LLM agents and supports reinforcement-learning self-play. The
built-in ruleset is OpenStratCore's own real-time hex-grid land-warfare ruleset, carried as data
(rules-as-data; see `config/`). Released under the **Apache-2.0** license.

## Design Principles
- **Modular**: a single deterministic Rust core + pluggable boundaries (rules-as-data, swappable probability providers, optional PyO3/wasm/native bindings, editable & replaceable map/scenario/rule assets).
- **AI-oriented**: built for AI from the ground up — structured tool interfaces for LLM agents + Gym/PettingZoo environments, supporting human-vs-machine, machine-vs-machine, and **self-play training**.
- **Graphical editor + resource model**: maps, scenarios, and rules each split into **built-in defaults** (read-only baselines) and **user resources** (authored in the editor or imported from text/JSON, and exportable). See `docs/RESOURCES.md` and `web/src/editor/`.

## What It Is
- A **real-time** (not turn-based) hex-grid wargame: simultaneous orders, first-arrival resolution, fixed-duration state transitions (75s/150s/300s), and dice/table-based adjudication.
- The engine is a **deterministic, seedable discrete-event simulator**: the same `seed + command stream` always replays into the same game — the foundation for replay and debugging.
- Three-layer architecture: **Rust core** → PyO3 (RL/LLM) + wasm (browser frontend) + an optional thin service (networked PvP).

## Repository Map
```
crates/openstratcore-core   Rust rules core (deterministic DES, rules-as-data, probability providers)
crates/openstratcore-py     PyO3 bindings (the RL / LLM harness for Python)
crates/openstratcore-wasm   wasm-bindgen bindings (run the engine locally in the browser)
python/                PettingZoo env, runnable self-play PPO example, LLM agent, probability learning
web/                   TS/React + PixiJS frontend (rendering / map·scenario·rule editors / replay viewer)
schemas/               JSON Schemas for map / scenario / rules / replay / LLM tools (the contract source)
config/                rules config (rules.default.json + tables/ — 18 adjudication tables)
scenarios/             example maps & scenarios (incl. the winnable RL scenario rl_duel)
prompts/               LLM commander system prompts + observation/tool-call samples
assets/                unit/terrain sprites + manifest
tools/                 sprite generation / Tiled import / validation / review scripts
docs/                  ARCHITECTURE · WORKFLOWS · ROADMAP · RESOURCES · RL · rules
```

## Quick Start (Local Setup After Cloning)
Pick one path by use case — the three are independent, ordered from the lightest to the heaviest toolchain:

**① Library / RL research (no extra toolchain, runs out of the box)** — needs only **Python 3.11+**:
```bash
cd python && pip install -e .          # pure Python (hatchling), no Rust build
python examples/selfplay_ppo.py --backend mock --total-steps 20000   # self-play smoke
```
`mock` is a pure-Python reference backend. To drive the environment with the deterministic **Rust core**, see ②.

**② Full engine (deterministic Rust core + Python bindings)** — needs **Rust (stable)** + Python + `make`:
```bash
pip install maturin
maturin develop -m crates/openstratcore-py/Cargo.toml   # build the core into the active Python env
make verify-all                                         # full regression gate (same as CI; should be all-green)
```

**③ Playable browser demo (Web frontend + wasm core)** — needs Rust + `wasm-pack` + **Node 20+**:
> Players can simply download the prebuilt bundle from **GitHub Releases** and serve it with `python -m http.server` — no toolchain required.
> To build it yourself: `wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg`,
> then `cd web && npm install && npm run build` (the build must run from a path **without spaces**). See [`web/README.md`](web/README.md).

The three end-to-end reinforcement-learning workflows (self-play / vs. scripted baseline / LLM matches) are in [`docs/RL.md`](docs/RL.md);
the development & contribution workflow is in `docs/WORKFLOWS.md`.

## Optional Integrations (LLM Commanders · Sprite Generation)
These are **optional** capabilities that require your own third-party API credentials; **the core engine, RL training, and the Web demo do not depend on them**:
- **LLM commander matches**: the `anthropic-api` / `openai-api` commanders call the respective official REST APIs and need `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` (see `docs/RL.md`).
- **Unit sprite generation**: the sprite scripts in `tools/` generate flat military-electronic-map-style art via an image model (neutral-background generation + automatic background removal), needing the relevant image-service credentials. The repository **already ships a generated sprite set** (`assets/generated/`), so regenerating is usually unnecessary.

## License
Released under the **Apache-2.0** license; see the full text in [`LICENSE`](LICENSE).
