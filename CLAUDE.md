# CLAUDE.md — OpenStratCore 开发规约

> Claude Code 每次会话都读这份文件。它是本仓库"宪法"。开发动作的细节见 `docs/WORKFLOWS.md`，
> 架构见 `docs/ARCHITECTURE.md`，路线见 `docs/ROADMAP.md`。

## 项目一句话
实时制六角格陆战兵棋。Rust 确定性内核 + PyO3/wasm 绑定 + Web 前端。支持 LLM-agent 对战与 RL 自博弈。

## 规则真源（动手前先定位）
完整规则**已在仓库内**，不要凭记忆/猜测：
- 本仓库内置规则集是 OpenStratCore **自有的原创**实时六角格兵棋系统。任何数值表落地进 `config/` 前**必须经双人交叉核验（两名独立作者对同一格值各自录入并一致）**。
- `config/tables/*.json` — 机器可读裁决表（引擎消费；带 `source.section` 与 `verified`）。
- `docs/rules/README.md`（小节↔表 映射）· `docs/rules/combat-model.md`（五条裁决流水线）· `docs/rules/coverage.md`（**进度真源**）。

## 八条硬规则（不可协商，hooks 会部分强制）
1. **确定性优先**：内核中任何随机性只能经 `openstratcore_core::rng::Rng` 取得，**禁止** `rand::thread_rng`、系统时间、`HashMap` 迭代顺序等非确定来源。`seed + 指令流` 必须可重放出同一局。
2. **规则即数据（rules-as-data）**：一切可调数值（速度、时长、射程、各裁决表、修正、概率 provider 配置）都从 `config/rules.*.json` 加载，**不准**硬编码进逻辑。改数值＝改配置 + 改 `schemas/rules.schema.json`，不改代码。
3. **契约先行**：跨语言/跨层的数据结构以 `schemas/*.json` 为唯一真源。改字段先改 schema，再改 Rust serde 结构与前端类型，最后跑 `make validate`。
4. **内核不 panic**：`openstratcore-core` 对外 API 返回 `Result<_, EngineError>`；不得在库代码里 `unwrap()/expect()/panic!`（测试除外）。`todo!()` 仅允许出现在明确标注的未实现 mechanic 中。
5. **战争迷雾正确性**：发给某方的观测只能包含该方依《观察/通视规则》真正可见的信息。任何"上帝视角"泄漏视为缺陷。
6. **测试随规则走**：每加/改一条规则，必须带至少一个确定性单元测试（固定 seed + 期望结果），并跑一局自博弈冒烟不崩。
7. **复盘不可破**：任何改动后 `make test` 必须包含"录制→重放→逐 tick 状态一致"的回归（见 `/replay-verify`）。破坏重放一致性的改动一律拒绝合并。
8. **小步提交**：一次 PR/一条工作流只做一件事（加一条规则 / 出一批贴图 / 一次重构）。大改先在 plan 模式拆解。

## 常用命令
- 构建/测试/静态检查：`make build` / `make test` / `make lint`
- 装 Python 扩展：`make py-dev`（maturin）
- 自博弈冒烟：`make selfplay`
- 出贴图：`make art`　校验样例 JSON：`make validate`
- Codex 复审当前 diff：`make review` 或 `/codex-review`

## 与 Codex 协作协议
本仓库把 **Codex 当作第二意见与外包工**，由你（Claude Code）经 bash 调用。

### 调用方式（必读，照此执行）
用 Bash 工具执行：
```
codex exec --skip-git-repo-check "<任务prompt>" < /dev/null
```
- 当前可能在**非 git 目录**，必须加 `--skip-git-repo-check`；`< /dev/null` 避免它卡在读 stdin。
- 这是**非交互模式**（别名 `codex e`），**已登录、模型 gpt-5.5**，直接返回结果即可用；**交互式 TUI 不要调**。
- 输出末尾若有 `�ɹ�: ����ֹ PID...` 乱码，是退出时清理子进程的 GBK 提示，**忽略即可**。
- Bash 工具的**超时设到 120000ms 以上**。
- 封装好的两个脚本已用此形式：`tools/codex_gen.sh`（生成/重构）、`tools/codex_review.sh`（复审）。

工作方式：
- **复审**：完成一段实现后，对 diff 跑 `bash tools/codex_review.sh <base>`（默认只读沙箱），把 Codex 指出的问题逐条评估——**采纳与否你来判断**，不照单全收，也不无视。
- **生成/重构**：边界清晰的机械改动可委托 `bash tools/codex_gen.sh "<任务>"`（见脚本，含 `--json`/`-o`）。委托前写清楚验收标准；回灌后**你负责**对照 schema/测试验证。
- **出图**：贴图经 Codex 驱动 `gpt-image-2` 生成（走 Codex 订阅），见 `/gen-art`。
- Codex 自身的规约在 `AGENTS.md`，与本文件保持一致；改其一通常要同步另一。

## 美术协议
- 风格：**现代军用电子地图 / 扁平矢量**（NATO APP-6 风格符号、有限调色板、等高/地物色块、清晰 UI）。详见 `assets/prompts/art_pack.yaml`。
- `gpt-image-2` 不支持透明背景：**中性底出图 → `tools/bg_remove.py` 自动抠底 → 写入 `assets/generated/` 并更新 `manifest.json`**。
- `assets/generated/` 由流程产出，**禁止手改**（hook 会拦截对该目录的写入）。

## 加一条规则怎么做
走 `/add-rule`（见 `docs/WORKFLOWS.md`）：① 在 plan 模式从规则原文摘出可判定语义 → ② 改 `rules.schema.json` 与 `config/rules.default.json`（新数值）→ ③ 在 `openstratcore-core` 对应 mechanic 实现，随机走 `prob` provider → ④ 写确定性测试 → ⑤ `make test && make lint` → ⑥ `/codex-review` → ⑦ `/selfplay` 冒烟 → ⑧ 更新 `docs/` 与规则覆盖清单。

## 目录边界
- 只在 `crates/`、`python/`、`web/`、`schemas/`、`config/`、`scenarios/`、`prompts/`、`tools/`、`docs/` 内改动。
- 不改：`assets/generated/`、`target/`、`node_modules/`、`.env`。
- 新增依赖前先说明理由（内核保持轻依赖）。

---

## 自主模式（Autonomous Mode）
**目标**：尽可能少打扰人地把游戏一步步做完、做对、可断点续跑。本仓库为此而生。

### 每次会话的循环
1. **读 `docs/rules/coverage.md`** → 找第一个未勾选 / `[~]` 项。
2. 到 **`TASKS.md`** 找对应 `T` 号，确认其 `dep:` 前置都已绿。
3. **若该任务含数值表**：先做**双人交叉核验**——两名独立作者各自录入同一组格值、比对一致后**只采用确认无误的数字**；据此写/订正 `config/`。
4. **按任务的执行标签选模型**（见「模型分配」）：🟡 Codex → `bash tools/codex_gen.sh "<带验收标准>"`；🟢 Haiku → 派子代理；🔴 → `/model` 升 Opus 亲自实现。回灌/实现后你对照 schema/测试验证。
5. 跑该任务的 `done when` 命令（多为某个 `cargo test ...`）。
6. **绿** → `bash tools/codex_review.sh <base>` 复审 → 逐条评估并修 → `make verify-all` 全绿 → 在 coverage 勾选 → `git commit`（message 带 `T` 号）。
7. 回到第 1 步，继续下一项。

### 何时停下来问人（且仅在这些时候停）
- **规则真歧义**：内置规则对某机制确有多解、或两表冲突且无法判定。
- **红门修不动**：同一失败你已连续尝试 ~3 次仍无法让 `make verify-all` 变绿。
- **越界/破坏性**：需要改 `目录边界` 之外、或会破坏重放一致性而无替代方案。
其余情况**一律继续推进**，不要为每一步征求许可。提问时给出：卡点、你试过什么、可选方案 A/B。

### 续跑纪律（compact / 换会话都不丢进度）
- 进度只看 **coverage 勾选 + git log**，不依赖会话记忆。
- **一任务一提交**：绿一个提交一个。永远保持 `main` 可 `make verify-all` 通过。
- 上下文吃紧时：把"读长文件 / 跑大检索 / 批量整理"**外包给 subagent**，主 agent 只接收结论。
- 不确定从哪续：跑 `git log --oneline -15` + 读 coverage 顶部第一个未勾项即可恢复。

### 委托速查
> Codex 调用统一形如：`codex exec --skip-git-repo-check "<prompt>" < /dev/null`（非交互、gpt-5.5、超时≥120000ms、末尾 GBK 乱码忽略）。

| 场景 | 怎么做 |
|---|---|
| 写/重构边界清晰的代码 | `bash tools/codex_gen.sh "<任务+验收>"` → 你验证 |
| 提交前复审 diff | `bash tools/codex_review.sh <base>` → 逐条自判 |
| 核验数值表 | 双人交叉核验：两名独立作者各自录入并比对一致 |
| 出贴图 | `/gen-art`（Codex→gpt-image-2→抠底） |
| 大检索/读长文档 | 派 subagent，回传摘要 |

## 模型分配（省 Claude 额度）
$100 Max plan 有 **5 小时滚动用量上限**，Opus 烧得最快。原则：**Opus 只用于真正难的步骤**，中等活外包 Codex（走 OpenAI 额度，**完全不吃 Claude 额度**），琐事下放 Haiku。

四档执行者：
| 档 | 用在哪 | 怎么调用 |
|---|---|---|
| 🔴 **Opus 4.8（high/max 思考）** | 硬、易错、架构性、最终判断：裁决流水线(直瞄对车/间瞄/防空)、两段式结果表录入、通视/迷雾、replay 一致性内核、schema 契约、集成调试 | `/model` 切到 opus（高思考档）；做完切回 |
| 🟡 **Codex（gpt-5.5）** | 中等、边界清晰的实现/重构/UI/胶水：多数机械 mechanic、编辑器界面、绑定/胶水、测试脚手架、文档生成 | `bash tools/codex_gen.sh "<任务+验收>"` |
| 🟢 **Haiku** | 简单、低风险、机械：跑 `make validate`/检查、改小配置、勾 coverage、生成 fixture | 派子代理（已 pin haiku），或主会话 `/model` 切 haiku |
| ⚪ **Sonnet（默认驱动）** | 跑自主循环本身（选下一任务、跑 make、勾选、提交）；比 Opus 省 | 默认主模型 |

**机制**
- 主模型用 `/model` 切换（或启动 `--model` / 环境变量 `ANTHROPIC_MODEL`）。默认 **Sonnet** 驱动循环，遇 🔴 任务临时升 **Opus 高思考**，完成后切回。
- 子代理模型已写进 `.claude/agents/*.md` 的 `model:`：`art-director`=haiku，`reviewer`/`rl-engineer`/`rules-engineer`=sonnet。
- 🟡 任务一律经 `tools/codex_gen.sh` 外包给 Codex；提交前的复审也走 Codex（`tools/codex_review.sh`），**只有复审结论有争议时**才升 Opus 拍板。

**省额度循环**：每个任务先看 `TASKS.md` 里它的执行标签 → 🟡 给 Codex、🟢 给 Haiku 子代理、🔴 才升 Opus；尽量把"实现"外包 Codex、"校验/勾选"下放 Haiku。
**触顶兜底**：若接近 5h 上限，把当前 🔴 任务临时降级为"Codex 出实现 + Opus 仅复核关键断言/数值"，或干脆暂停到额度恢复——进度在 coverage 勾选 + git log，安全可续，不丢工作。

## 完成定义（Definition of Done）
任一**代码任务**完成的硬标准（与 `TASKS.md` 顶部一致）：
1. `make build` + `make lint`（clippy -D warnings）通过。
2. ≥1 个确定性单测（固定 seed + 期望值）。
3. `make test` 全绿，含"录制→重放→逐 tick 一致"回归未破。
4. 数值在 `config/`、schema 已改、`make validate` 通过、**且已经双人交叉核验（两名独立作者各自录入并一致）**。
5. 内核无新增 `unwrap/expect/panic`；观测无上帝视角泄漏。
6. `make verify-all` 全绿后 `git commit`。
> 经验法则：宁可慢、每步带门，也不要"一把梭"。引擎的价值在于**可信**——错误一旦累积且互相掩盖，远比慢更贵。
