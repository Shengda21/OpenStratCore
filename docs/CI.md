# CI — headless `make verify-all`

How to run the full regression gate (`make verify-all`) in a headless / CI
environment, and the auth model for the agents (Claude Code, Codex) that drive
development — **on subscriptions, not a metered API key**.

## What the gate runs
`make verify-all` chains, stopping at the first failure:

```
fmt-check → build → lint (clippy -D warnings) → test → validate → selfplay-smoke
```

It must exit `0` before every commit (CLAUDE.md「完成定义」, hard rule #7). The
gate itself is **pure Rust + Python** — it needs no Claude/Codex credentials.

## Toolchain prerequisites
- **Rust** stable with `clippy` + `rustfmt`:
  `rustup toolchain install stable && rustup component add clippy rustfmt`.
  On Windows the default `*-pc-windows-msvc` target links with the MSVC linker, so
  install **VS C++ Build Tools** (the *VCTools* workload + a Windows SDK). `rustc`
  finds `link.exe` via vswhere; a stray Unix `link.exe` on `PATH` does not interfere.
- **GNU make** (e.g. `ezwinports.make` on Windows).
- **Python ≥ 3.11** with:
  - `jsonschema` — `make validate`
  - `numpy`, `gymnasium`, `torch` — `make selfplay-smoke` (PPO on the pure-Python
    mock backend; `torch` is the heaviest dep)
  - install: `pip install jsonschema numpy gymnasium torch`
  - optional, NOT in the gate: `pettingzoo` (RL extra), `anthropic`/`openai` (LLM extra)
  - the Makefile runs the examples with `PYTHONPATH=.` from `python/`; **no editable
    install is required** for the gate, and the gate does **not** need the Rust
    extension built into Python (`make py-dev`) — `selfplay-smoke` uses the mock backend.

## Auth model — subscription, NOT a metered API key
Development is driven by **Claude Code** (Anthropic) and **Codex** (OpenAI `gpt-5.5`)
via per-seat **subscriptions**, not per-token API billing.

- **Do NOT set `ANTHROPIC_API_KEY`** (in CI or locally). Setting it switches Claude
  Code to metered token billing. Verify it is unset before driving the loop.
- For a headless Claude Code agent (e.g. a scheduled runner of the autonomous loop),
  mint a long-lived OAuth token once on a machine that holds the subscription:
  ```sh
  claude setup-token            # interactive; prints a CLAUDE_CODE_OAUTH_TOKEN
  ```
  then in CI:
  ```sh
  export CLAUDE_CODE_OAUTH_TOKEN=...   # from `claude setup-token`
  unset  ANTHROPIC_API_KEY             # ensure metered billing is OFF
  ```
- **Codex** (`codex exec`, via `tools/codex_gen.sh` / `tools/codex_review.sh`) uses its
  own logged-in subscription session, non-interactively. Note: the Codex sandbox may
  not see this repo's files (e.g. on a VMware shared folder), so `codex_gen` must
  inline the needed file contents into the prompt; `codex_review` passes the diff in
  the prompt and is unaffected.

## Running the gate
```sh
make verify-all          # full gate; exit 0 = safe to commit
# or individual steps:
make fmt-check build lint test validate selfplay-smoke
```

## Repo gotchas
- JSON files are **UTF-8** (rule tables carry Chinese text). Tooling reads them as
  UTF-8 explicitly — never rely on the platform default (GBK/cp936 on Chinese Windows),
  which corrupts them.
- `cargo build --workspace` builds the PyO3 cdylib, which needs a Python install
  discoverable by pyo3's build script (the same interpreter you run the examples with).
- Progress is tracked in `docs/rules/coverage.md` (checkboxes) + `git log`; `main` must
  stay `make verify-all`-green after every task commit.
