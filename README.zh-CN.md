[English](README.md) | **简体中文**

# OpenStratCore

开源、对 AI 友好、模块化的**实时制**六角格陆战兵棋推演平台。为下一代兵棋而建：可接入 LLM-agent，
可做强化学习自博弈。内置规则集是 OpenStratCore 自有的一套实时六角格陆战规则，以数据形式承载
（rules-as-data，见 `config/`）。以 **Apache-2.0** 许可证开源。

## 设计取向
- **模块化**：单一确定性 Rust 内核 + 可插拔边界（规则即数据、概率 provider 可换、PyO3/wasm/native 绑定可选、地图/想定/规则资源可编辑可替换）。
- **智能化**：原生面向 AI——LLM-agent 结构化工具接口 + Gym/PettingZoo 环境，支持人机/机机与**自博弈训练**。
- **图形化编辑器 + 资源模型**：地图/想定/规则三类资源分**内置默认**（只读基线）与**用户资源**（编辑器手动新建或从文本/JSON 导入、可导出）。见 `docs/RESOURCES.md`、`web/src/editor/`。

## 这是什么
- **实时制**（非回合制）六角格兵棋：同时下令、先到先裁、固定时长状态转换（75s/150s/300s）、骰子查表裁决。
- 引擎是一个**确定性、可设种子（seedable）的离散事件模拟器**：同一 `seed + 指令流` 必然重放出同一局（这是复盘与调试的基石）。
- 三层架构：**Rust 内核** → PyO3（RL/LLM）+ wasm（浏览器前端）+ 可选瘦服务（联网 PvP）。

## 仓库地图
```
crates/openstratcore-core   Rust 规则内核（确定性 DES、rules-as-data、概率 provider）
crates/openstratcore-py     PyO3 绑定（给 Python 的 RL / LLM harness）
crates/openstratcore-wasm   wasm-bindgen 绑定（浏览器本地直跑引擎）
python/                PettingZoo 环境、可跑的 self-play PPO 样例、LLM-agent、概率学习
web/                   TS/React + PixiJS 前端（渲染 / 地图·想定·规则编辑器 / 复盘查看器）
schemas/               地图 / 想定 / 规则 / 复盘 / LLM 工具 五套 JSON Schema（契约真源）
config/                规则配置（rules.default.json + tables/ 18 张裁决表）
scenarios/             示例地图与想定（含 RL 用的可赢想定 rl_duel）
prompts/               LLM 指挥官系统提示词 + 观测/工具调用样例
assets/                单位/地形贴图 + manifest
tools/                 出图 / Tiled 导入 / 校验 / 复审 等脚本
docs/                  ARCHITECTURE · WORKFLOWS · ROADMAP · RESOURCES · RL · rules
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
> 再 `cd web && npm install && npm run build`（构建须在**无空格路径**下进行）。详见 [`web/README.md`](web/README.md)。

强化学习的三条端到端流程（自博弈 / 对脚本基线 / LLM 对战）见 [`docs/RL.md`](docs/RL.md)；
开发与贡献流程见 `docs/WORKFLOWS.md`。

## 可选集成（LLM 指挥官 · 贴图生成）
以下为**可选**能力，需要你自己的第三方 API 凭证；**核心引擎、RL 训练与 Web demo 都不依赖它们**：
- **LLM 指挥官对战**：`anthropic-api` / `openai-api` 指挥官直连各自官方 REST API，分别需要
  `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`（见 `docs/RL.md`）。
- **单位贴图生成**：`tools/` 的出图脚本经图像模型生成扁平军用电子地图风格贴图（中性底出图 + 自动抠底），
  需相应图像服务凭证。仓库**已内置一套生成好的贴图**（`assets/generated/`），通常无需重跑。

## 许可证
本项目以 **Apache-2.0** 许可证发布，全文见 [`LICENSE`](LICENSE)。
