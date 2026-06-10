# Task runner. The .claude skills call these targets so behavior is identical
# whether a human, Claude Code, or Codex runs them.
.PHONY: build test lint fmt fmt-check py-dev selfplay selfplay-smoke llm-match art review validate verify-all clean

build:        ## build the Rust core + bindings
	cargo build --workspace

test:         ## run Rust + Python tests
	cargo test --workspace
	cd python && pytest -q || true

lint:         ## clippy with warnings as errors
	cargo clippy --workspace --all-targets -- -D warnings

fmt:          ## format Rust
	cargo fmt --all

py-dev:       ## build the PyO3 extension into the active venv (editable)
	cd python && maturin develop -m ../crates/openstratcore-py/Cargo.toml

selfplay:     ## smoke-train self-play PPO on the mock backend
	cd python && PYTHONPATH=. python examples/selfplay_ppo.py --backend mock --total-steps 20000

llm-match:    ## run one LLM match (Claude vs Codex, via your subscriptions; no API key)
	cd python && PYTHONPATH=. python examples/llm_agent.py --red claude --blue codex

art:          ## generate sprites via Codex+gpt-image-2 then auto-remove backgrounds
	python tools/gen_art.py --pack assets/prompts/art_pack.yaml --out assets/generated

review:       ## non-interactive Codex review of the working diff vs main
	bash tools/codex_review.sh main

validate:     ## validate all sample JSON against the schemas
	python tools/validate.py
	@echo "validating config/tables/*.json well-formedness…"
	@for f in config/tables/*.json; do python -c "import json,sys;json.load(open('$$f', encoding='utf-8'))" || exit 1; done
	@echo "tables OK"

fmt-check:    ## fail if Rust is not formatted (CI gate)
	cargo fmt --all -- --check

selfplay-smoke: ## very short self-play run, just to prove it doesn't crash
	cd python && PYTHONPATH=. python examples/selfplay_ppo.py --backend mock --total-steps 2000

sim-smoke:    ## scripted closed-loop slice smoke (skips gracefully if the py ext isn't built)
	python tools/sim_smoke.py

integration:  ## full-mechanics end-to-end suite over the JSON/wheel API (skips if ext not built)
	python tools/integration_battle.py

verify-all:   ## FULL regression gate — must be green before every commit
	@echo "==> fmt-check"   && $(MAKE) fmt-check
	@echo "==> build"       && $(MAKE) build
	@echo "==> lint"        && $(MAKE) lint
	@echo "==> test"        && $(MAKE) test
	@echo "==> validate"    && $(MAKE) validate
	@echo "==> selfplay-smoke" && $(MAKE) selfplay-smoke
	@echo "==> sim-smoke"   && $(MAKE) sim-smoke
	@echo "==> integration" && $(MAKE) integration
	@echo "ALL GREEN ✅  (safe to commit)"

clean:
	cargo clean; rm -rf python/build python/*.egg-info
