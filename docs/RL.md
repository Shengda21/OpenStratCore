# RL / LLM 研究平台

OpenStratCore 原生面向 AI：同一个确定性内核，既可被 **RL 自博弈**驱动，也可被 **LLM 指挥官**驱动。
本文是三条端到端流程的真源。

## 环境

`python/openstratcore_env/` 提供一个 **PettingZoo `ParallelEnv`** 兼容环境，两个 agent：`red` / `blue`。
- 观测：每方按《观察/通视》规则的**战争迷雾**投影（10 维），无上帝视角。
- 动作：`Discrete(5)`——0 待命 / 1 移动 −q / 2 移动 +q / 3 直瞄开火 / 4 停止进入射击姿态。
  （动作控制该方**领队棋子**；多棋协同是后续工作。）
- 奖励整形（训练信号，**非**游戏规则）：歼敌 +／损失 −／接近目标 +／时间惩罚 −／胜负 ±10。

两种后端：

| 后端 | 工具链 | 用途 |
|---|---|---|
| `mock`（默认） | **纯 Python**，开箱即跑 | 一维六角线小遭遇战，快速验证训练回路；自博弈 |
| `rust` | 需 `maturin` 构建 PyO3 扩展 | **真实确定性内核**，经 `observe()`/JSON 指令接口接入；支持 self-play 与 vs-脚本 |

构建 rust 后端扩展：
```bash
pip install maturin
maturin develop -m crates/openstratcore-py/Cargo.toml   # 或见 docs/CI.md 的 build+install wheel
```

## 1) 自博弈（self-play）

纯 Python，**无需任何编译**：
```bash
cd python && pip install -e .
PYTHONPATH=. python examples/selfplay_ppo.py --backend mock --total-steps 20000
```
真实内核自博弈（需先构建 rust 扩展）：
```bash
PYTHONPATH=. python examples/selfplay_ppo.py --backend rust --opponent self --total-steps 100000 --seed 1
```

## 2) 能赢的想定：`rl_duel` —— 训练红方稳定胜过脚本基线

仓内自带一个**可学、可稳定取胜**的想定 `scenarios/rl_duel.scenario.json`：红方领队在快速路一侧、
离控制点更近，须学会"沿路抢占并守住控制点"，对手是仓内确定性的 `ScriptedCommander`（停-打-夺控）。

```bash
cd python
PYTHONPATH=. python examples/selfplay_ppo.py \
  --backend rust --opponent scripted --scenario rl_duel.scenario.json \
  --total-steps 60000 --seed 1 --ent-coef 0.02 --save runs/rl_duel_baseline
```

期望（seed 1，确定性可复现）：约 **8k 步**红方即达 `W[r20/b0]`（最近 20 局全胜），并稳定保持；
末段 `ep_return≈+13`、`fires 20 hits 20`（学会精确开火）、局长由 ~30 缩短到 ~9 tick（决定性速胜）。
checkpoint 存到 `runs/rl_duel_baseline/selfplay_ppo.pt`（预训练基线随 GitHub Release 分发）。

> 为什么默认的 `demo_skirmish` 不适合作"能赢"基准：env 动作只沿 q 轴移动，而该想定在 (1,1) 设了蓝方路障、
> 控制点又靠蓝方寻路一侧——单领队的可学策略够不到控制点。`rl_duel` 专为"单领队可赢"而调平。

## 3) LLM 指挥官对战

让两个 LLM（或 LLM vs 脚本）按结构化工具接口（`schemas/llm_tools.schema.json`）对弈：
```bash
cd python
PYTHONPATH=. python examples/llm_agent.py --red claude --blue codex
```
指挥官走**订阅鉴权**（无 API Key）；可选 `anthropic-api`/`openai-api` 直连按量计费路径，适合大批量对战。
观测/工具调用样例见 `prompts/examples/`。

## 复现与确定性
- 固定 `--seed` ⇒ 同一 backend/opponent 组合下 rollout 可复现（`torch`/`numpy`/env reset 均按 seed）。
- 加载 checkpoint 评测：构造 `ActorCritic(OBS_DIM, N_ACTIONS)` → `load_state_dict(torch.load(...))` → 用
  `make_env(backend="rust", opponent="scripted", scenario_file="rl_duel.scenario.json")` 跑一局。
- 想定可换：`--scenario <file>`（默认 `demo_skirmish.scenario.json`；mock 后端忽略此参数）。
