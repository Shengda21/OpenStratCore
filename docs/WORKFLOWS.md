# Dynamic Workflows

一组**可复用、可组合**的开发动作。每条工作流既是 `.claude/skills/<name>/SKILL.md`（在 Claude Code 里用 `/<name>` 触发），
也可被别的工作流当子步骤调用（"动态"= 组合而非线性脚本）。工作流之间靠**门禁（gates）**串联：
`make test`、`make lint`、`make validate`、`/codex-review`、`/replay-verify` 都是门禁；未过门禁不进入下一步。

## 角色（子代理，`.claude/agents/`）
| 子代理 | 职责 | 何时 fork |
|---|---|---|
| `rules-engineer` | 在 Rust 内核实现/修改规则 mechanic + 写确定性测试 | `/add-rule` 主体 |
| `rl-engineer` | PettingZoo 环境、观测/动作空间、训练脚本 | `/selfplay`、RL 相关 |
| `art-director` | 美术提示词、Codex 出图、抠底、manifest | `/gen-art` |
| `reviewer` | 驱动 `codex review` 并复核结论 | `/codex-review` |
| 内置 `Explore` | 只读探索内核找改动点 | 任何"先读懂代码"环节 |
| 内置 `Plan` | 把规则原文/大改拆成步骤，不执行 | 每条工作流的开头 |

子代理有独立上下文窗口，用于隔离繁重探索/实现，保持主对话聚焦、省 token。

## 工作流模板
每条工作流按统一结构描述：**触发 / 输入 / 步骤(含 fork 与门禁) / 产出 / 失败回退**。

---

### 1. `/add-rule` — 增加或修改一条游戏规则（核心循环）
- **触发**：`/add-rule <规则编号或名称>`（如 `/add-rule 8.3 行进间射击`）。
- **输入**：规则原文片段（来自规则说明）、受影响的单位/状态。
- **步骤**
  1. `Plan` 子代理：从原文摘出**可判定语义**（触发条件、时长、数值、随机、边界、与其他规则的互斥），列成验收清单。
  2. 若涉及新数值/表：改 `schemas/rules.schema.json` → 在 `config/rules.default.json` 填值 → `make validate`（门禁）。
  3. `Explore` 子代理：定位 `openstratcore-core` 的实现点（`mechanics.rs`/`combat.rs`/`movement.rs`/`engine.rs`）。
  4. `rules-engineer` 子代理：实现 mechanic；随机一律经 `prob::ProbProvider`；不 panic；不硬编码数值。
  5. 写**确定性单测**（固定 seed + 期望结果）覆盖典型与边界。
  6. 门禁：`make test && make lint`。
  7. 门禁：`/codex-review`（diff 复审，逐条评估）。
  8. 门禁：`/selfplay --total-steps 5000`（冒烟，不崩即可）+ `/replay-verify`（重放一致）。
  9. 更新 `docs/` 与"规则覆盖清单"（见末尾）。
- **产出**：内核实现 + 测试 + 配置 + 文档；一次聚焦提交。
- **失败回退**：任一门禁红 → 退回对应步骤；语义有歧义 → 回到步骤 1 找人确认，**不臆造**。

### 2. `/new-scenario` — 制作地图 + 想定
- **触发**：`/new-scenario <名称>` 或 `/new-scenario --import map.tmx`。
- **步骤**
  1. 地图：直接写/改 `scenarios/maps/<name>.map.json`（AI 可文本生成）；或 `python tools/tmx_import.py map.tmx > scenarios/maps/<name>.map.json`。
  2. 想定：写 `scenarios/<name>.scenario.json`（双方编成/摆放/目标/胜负/时限/预置设施）。
  3. 门禁：`make validate`（两份都过 schema）。
  4. 自检：`python tools/sim_smoke.py scenarios/<name>.scenario.json`（脚本对手对打 N 步不崩、可终局）。
- **产出**：一对可加载的地图+想定文件。

### 3. `/gen-art` — 生成贴图（Codex + gpt-image-2 + 抠底）
- **触发**：`/gen-art [资产组，如 terrain|units|ui|all]`。
- **步骤**（`art-director` 子代理）
  1. 从 `assets/prompts/art_pack.yaml` 取该组的**详细 content+style 提示词**（统一 S1 风格 + 固定调色板/尺寸）。
  2. 经 Codex 驱动 `gpt-image-2` 出图（走订阅）：`python tools/gen_art.py --pack ... --group ...`。
  3. 单位/图标类**自动抠底**：`tools/bg_remove.py`（gpt-image-2 无透明）。
  4. 归一化命名、尺寸校验，写 `assets/generated/` 并更新 `assets/generated/manifest.json`。
  5. 门禁：人工/缩略图抽检风格一致；不一致则微调提示词重出（小批，省额度）。
- **产出**：成套贴图 + manifest。
- **注意**：`assets/generated/` 禁手改（hook 拦截）。

### 4. `/codex-review` — 非交互 Codex 复审
- **触发**：`/codex-review [base 分支，默认 main]`。
- **步骤**（`reviewer` 子代理）：`bash tools/codex_review.sh <base>`（`codex review --base`，只读沙箱）→ 解析问题清单 → **逐条判断采纳/驳回并说明理由** → 需修的回到来源工作流。
- **产出**：复审结论 + 处置记录。

### 5. `/selfplay` — RL 自博弈训练 / 回归
- **触发**：`/selfplay [--backend mock|rust] [--total-steps N]`。
- **步骤**（`rl-engineer` 子代理）：确保环境与内核 API 对齐 → `python examples/selfplay_ppo.py ...` → 记录到 `runs/`。冒烟模式只验证"能跑、回报不发散、不崩"。
- **产出**：训练曲线/检查点；或冒烟通过标记。
- **升级路径**：换算法＝改 `examples/selfplay_ppo.py`（单文件）或新增 `examples/<algo>.py`，环境接口不变。

### 6. `/llm-match` — LLM 指挥官对战（人机 / LLM 自博弈）
- **触发**：`/llm-match [--red claude|codex|anthropic-api|openai-api|scripted|human] [--blue ...]`。默认 `--red claude --blue codex`。
- **步骤**：加载地图+想定 → 每个决策 tick 为各方构造**战争迷雾观测**（`prompts/commander_system.md` + 工具 schema）→ 解析工具调用为引擎指令 → 推进 → 录制复盘。
- **产出**：一局对战 + `*.replay.json`。
- **账号**：`claude`/`codex` 走你的 Max/ChatGPT **订阅**（无 Key，消耗订阅用量额度，适合可观看/评测对局）；`anthropic-api`/`openai-api` 为可选**按量计费**路径（大批量用）。零工具冒烟用 `--red scripted --blue scripted`。

### 7. `/calibrate-prob` — 概率学习（更新 provider 参数）
- **触发**：`/calibrate-prob <表名> [--data runs/outcomes.jsonl] [--prior expert.json]`。
- **步骤**：`python python/prob_learning/calibrate.py ...` 用①复盘 outcome 数据 + ②专家先验做 Dirichlet-Multinomial 后验更新 → 产出新的 `rules.prob` 参数块 → 写回 `config/rules.<variant>.json` → `make validate` → `/replay-verify`（确认确定性仍成立）。
- **产出**：校准后的规则配置变体。

### 8. `/replay-verify` — 复盘确定性回归
- **触发**：`/replay-verify [replay.json]`。
- **步骤**：用 `header.seed + commands` 重跑内核 → 与录制时的周期快照逐 tick 比对 → 不一致即报告首个分歧 tick（**确定性被破坏 = 阻断**）。
- **产出**：一致/分歧报告；CI 必跑。

---

## 组合示例（"动态"）
- 上线"间瞄射击"：`/add-rule 9 间瞄` 内部会调用 `/codex-review`、`/selfplay`、`/replay-verify` 作为门禁；若间瞄需要新单位图标，再触发 `/gen-art units`。
- 平衡性调参：`/llm-match` 批量产 outcome → `/calibrate-prob 直瞄对车辆` → 新配置回到 `/llm-match` 复测。

## 规则覆盖清单（随 /add-rule 勾选）
- [x] 1 机动（车辆/人员/行军、状态机、75s 转换）— 纵向切片
- [x] 6 通视 / 7 观察（战争迷雾）— 纵向切片
- [x] 8 直瞄射击（武器展开/冷却/查表/修正）— 纵向切片
- [x] 5 夺控（胜负）— 纵向切片
- [ ] 2 上下车  [ ] 3 掩蔽  [ ] 4 堆叠
- [ ] 9 间瞄  [ ] 10 巡飞弹  [ ] 11 无人机  [ ] 12 武装直升机  [ ] 13 无人战车  [ ] 14 引导射击  [ ] 15 同格交战
- [ ] 16 运输直升机  [ ] 17 聚合解聚  [ ] 18 防空  [ ] 19 侦察校射  [ ] 20 工事  [ ] 21 雷场  [ ] 22 支援(天基侦察)

## Codex 调用约定（所有用到 Codex 的工作流）
统一用：`codex exec --skip-git-repo-check "<prompt>" < /dev/null`（非交互、gpt-5.5、超时≥120000ms、末尾 GBK 乱码忽略、非 git 目录必加 `--skip-git-repo-check`）。封装：`tools/codex_gen.sh`、`tools/codex_review.sh`。
