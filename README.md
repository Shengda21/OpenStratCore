# OpenStratCore

开源、对 AI 友好、模块化的实时制陆战兵棋推演平台。为下一代兵棋而建：可接入 LLM-agent 与 AI、可做自博弈训练。内置规则集是 OpenStratCore 自有的一套实时六角格陆战规则，以数据形式承载（rules-as-data，见 `config/`）。

> 名称可随时改：根目录、Cargo 包 `openstratcore-core/py/wasm`、Python 包 `openstratcore_env`。

## 设计取向
- **模块化**：单一确定性 Rust 内核 + 可插拔边界（规则即数据、概率 provider 可换、PyO3/wasm/native 绑定可选、地图/想定/规则资源可编辑可替换）。
- **智能化**：原生面向 AI——LLM-agent 结构化工具接口 + Gym/PettingZoo 环境，支持人机/机机与**自博弈训练**。
- **图形化编辑器 + 资源模型**：地图/想定/规则三类资源分**内置默认**（内置规则与跑通最小集，只读）与**用户资源**（编辑器手动新建或从文本/JSON 导入、可导出）。见 `docs/RESOURCES.md`、`web/src/editor/`。

## 这是什么
- **实时制**（非回合制）六角格兵棋：同时下令、先到先裁、固定时长状态转换（75s/150s/300s）、骰子查表裁决。
- 引擎是一个**确定性、可设种子（seedable）的离散事件模拟器**：同一 `seed + 指令流` 必然重放出同一局（这是复盘与调试的基石）。
- 三层架构：**Rust 内核** → PyO3（RL/LLM）+ wasm（浏览器前端）+ 可选瘦服务（联网 PvP）。

## 仓库地图
```
crates/openstratcore-core   Rust 规则内核（确定性 DES、rules-as-data、概率 provider）
crates/openstratcore-py     PyO3 绑定（给 Python 的 RL / LLM harness）
crates/openstratcore-wasm   wasm-bindgen 绑定（浏览器本地直跑引擎）
python/                PettingZoo 环境、可跑的 self-play PPO 样例、LLM agent、概率学习
web/                   TS/React + PixiJS 前端（渲染 / 地图&想定&规则编辑器 / 复盘查看器）骨架
schemas/               地图 / 想定 / 规则 / 复盘 / LLM 工具 五套 JSON Schema（契约真源）
config/                规则配置实例（rules.default.json）
scenarios/             示例地图与想定
prompts/               LLM 指挥官系统提示词 + 观测/工具调用样例
assets/                生成的贴图 + manifest；assets/prompts 为美术提示词包
tools/                 Codex 复审/生成包装脚本、gpt-image 出图+抠底、Tiled 导入
.claude/               Dynamic Workflows：skills(=斜杠命令) / agents(子代理) / settings(hooks+权限)
docs/                  ARCHITECTURE / WORKFLOWS / ROADMAP / RESOURCES / rules(规则库与覆盖清单)
```

## 快速开始（克隆后本地部署）
按用途选一条路——三条彼此独立，按所需工具链由轻到重：

**① 库 / RL 研究（零额外工具链，开箱即跑）** — 仅需 **Python 3.11+**：
```bash
cd python && pip install -e .          # 纯 Python（hatchling），不编译 Rust
python examples/selfplay_ppo.py --backend mock --total-steps 20000   # 自博弈冒烟
```
`mock` 是纯 Python 参考后端；要用确定性 **Rust 内核**驱动环境，见 ②。

**② 完整引擎（确定性 Rust 内核 + Python 绑定）** — 需 **Rust(stable)** + Python + `make`：
```bash
pip install maturin
maturin develop -m crates/openstratcore-py/Cargo.toml   # 把内核编进当前 Python 环境
make verify-all                                         # 全门回归（CI 同款，应全绿）
```

**③ 浏览器可玩 demo（Web 前端 + wasm 内核）** — 需 Rust + `wasm-pack` + **Node 20+**：
> 普通玩家可直接下 **GitHub Releases** 里的预构建包，`python -m http.server` 即开即玩，无需工具链。
> 自行构建：`wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg`，
> 再 `cd web && npm install && npm run build`（构建须在**无空格路径**下进行）。详见 `web/`。

开发动作（加规则 `/add-rule`、出图 `/gen-art`、复盘、`/codex-review` 等）见 `docs/WORKFLOWS.md`；
给 AI 助手的规约见 `CLAUDE.md`。

## 成本与账号（重要）
你是 Anthropic + OpenAI 双 Max 会员，因此**默认全程走订阅，不需要任何 API Key**：
- **Claude Code**（含无头 `claude -p`）和 **Codex CLI**（`codex exec` / `codex review`）都用你的**订阅鉴权**——无 Key、无按 token 计费，只消耗订阅用量额度。无头/CI 用 `claude setup-token` 生成一年期 `CLAUDE_CODE_OAUTH_TOKEN`；Codex 用 ChatGPT 登录或访问令牌。
- **LLM 对战**默认走 CLI 指挥官（`--red claude --blue codex`），即订阅路径，无 Key。
- **可选的按量计费路径**：`anthropic-api` / `openai-api` 指挥官直连 REST API，需 `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`，适合**大批量自动对战**。
- 经 Codex 的 `gpt-image-2` 出图同样走 **Codex 订阅**。`gpt-image-2` **不支持透明背景**，单位贴图统一"中性底出图 + 自动抠底"。
- ⚠️ 注意：若环境里设了 `ANTHROPIC_API_KEY`，`claude -p` 会**优先按 API 计费**而非订阅——想走订阅就别设它。自 2026-06-15 起，订阅下的 `claude -p` 用量从独立的 Agent SDK 额度池扣减。

## 许可证
本项目以 **Apache-2.0** 许可证发布，全文见 [`LICENSE`](LICENSE)。
