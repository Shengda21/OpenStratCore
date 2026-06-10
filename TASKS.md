# TASKS.md — 可自主执行的任务清单

> 配合 `docs/rules/coverage.md`（进度真源）与 `CLAUDE.md`「自主模式」段使用。
> **拓扑有序**：原则上从上往下做；`dep:` 标注的前置任务必须先绿。
> **每个任务格式**：`done when <可运行命令/测试>`。命令通过即视为完成 → 在 coverage 勾选 → `git commit`（一任务一提交，message 含任务号）。
> **委托纪律**（省主 agent 上下文，硬性建议）：
> - 边界清晰的实现/重构/样板 → `bash tools/codex_gen.sh "<含验收标准的任务>"`，回灌后**你**对照 schema/测试验证。
> - 每个任务实现完、commit 前 → `bash tools/codex_review.sh <base>` 复审 diff，逐条评估（采纳与否自判）。
> - 读长文件、跑大检索、批量转写 → **派 subagent** 去做并只回传结论，不要把原文灌进主上下文。
> - 出图 → `/gen-art`（Codex 驱动 gpt-image-2 + 自动抠底）。

## 模型分配（执行标签，省 Claude 5h 额度）
每条任务前的标签指明**用谁执行**：🔴 **Opus**（难/易错/架构/最终判断）· 🟡 **Codex**（中等机械活，外包 gpt-5.5，不吃 Claude 额度）· 🟢 **Haiku**（简单机械，如双人交叉核验数值、跑校验、勾 coverage）。驱动循环默认用 **Sonnet**，遇 🔴 才 `/model` 升 Opus。机制与触顶兜底见 `CLAUDE.md`「模型分配」。
- 调用 Codex：`bash tools/codex_gen.sh "<任务+验收>"`；复审 `bash tools/codex_review.sh <base>`。
- 数值核验：由两位作者各自独立录入同一数值并比对一致（双人交叉核验），差异需查清后才落地。

## 全局完成定义（Definition of Done，每个代码任务都要满足）
1. `make build` 通过；`make lint`（clippy -D warnings）通过。
2. 至少 1 个**确定性**单测：固定 seed + 期望结果（硬规则#6）。
3. `make test` 全绿，且含"录制→重放→逐 tick 一致"回归未被破坏（硬规则#7）。
4. 涉及数值的：数值在 `config/`，schema 已更新，`make validate` 通过；**且数值已经双人交叉核验（两名独立作者对同一格值各自录入并一致）**（硬规则#2/#3）。
5. 内核无新增 `unwrap/expect/panic`（硬规则#4）；观测无上帝视角泄漏（硬规则#5）。
6. `git commit`（小步，硬规则#8）。

---

## 阶段 0 — 地基闭环（先把"可重放 + 全量校验"打通）
- **T0.1** 〔🟡 Codex〕 接 `config/tables/*` 进 `rules.rs` 加载层 dep:无 — done when `cargo test -p openstratcore-core rules::` 通过且能反序列化全部 18 张表（写一个加载测试）。委托：codex_gen 写 loader + serde 结构。
- **T0.2** 〔🔴 Opus〕 `/replay-verify` 真落地：录一局 → 重放 → 逐 tick snapshot 一致 dep:T0.1 — done when `cargo test -p openstratcore-core replay::roundtrip` 通过。计时见 ARCHITECTURE「连续时间引擎约定」：整数 `Tick`、出队全序 `(time,seq)`。
- **T0.3** 〔🟢 Haiku〕 新增 `make verify-all`（见 Makefile）：fmt-check + build + clippy + test + validate + selfplay-smoke 一把过 dep:T0.2 — done when `make verify-all` 退出码 0。
- **T0.4** 〔🟢 Haiku〕 CI headless：用 `CLAUDE_CODE_OAUTH_TOKEN`（`claude setup-token` 生成）跑 `make verify-all`，**不**设 `ANTHROPIC_API_KEY` dep:T0.3 — done when 文档 `docs/CI.md` 写明且本地 `make verify-all` 绿。

## 阶段 1 — 纵向切片 MVP（对应 coverage M1）
- **T1.1** 〔🟡 Codex〕 机动状态机（5 态互斥 + 75s 转换 + 停止惩罚） dep:T0.3 src:三.1/`timings.json` — done when `cargo test mechanics::movement_states`。抢占/计时遵 ARCHITECTURE「连续时间引擎约定」（完成当前格再停=保留当前到达、作废后续；代次号失效）。委托 review。
- **T1.2** 〔🟡 Codex〕 车辆机动速度（地形系数×坡度系数；>5级/路障不可通行；沿路免修正） src:三.1.2/`terrain_movement.json` — done when `cargo test mechanics::vehicle_speed`（含丛林/居民/小河/大河/松软/坡度2-5 各一断言）。
- **T1.3** 〔🟡 Codex〕 人员机动（地形无关；高差>60m 半速；一/二级冲锋倍率与疲劳；75s 降疲劳） src:三.1.4 — done when `cargo test mechanics::infantry_move`。
- **T1.4** 〔🔴 Opus〕 通视（连线插值 + 居民/丛林+1 + 高看低+1） src:三.6 — done when `cargo test mechanics::line_of_sight`（造 3 个高程/地物用例：通/不通/相邻必通）。
- **T1.5** 〔🔴 Opus〕 观察 + 战争迷雾观测（完整 8 行表 + 掩蔽/地形减半；observe() 只回该方可见） src:三.7/`observation_distance.json` — done when `cargo test engine::fog_of_war` 断言"敌方不可见单位不出现在 observe 结果"。
- **T1.6** 〔🔴 Opus〕 直瞄对人员流水线 B dep:T1.4,T1.5 src:附1.1/1.7/1.8 — done when `cargo test combat::direct_vs_personnel`（固定 seed 验一次毁伤、一次压制、一次无效）。**先对附1.1/1.7/1.8 做双人交叉核验**。
- **T1.7** 〔🔴 Opus〕 直瞄对车辆流水线 A dep:T1.6 src:附1.2/1.4/1.5/1.6 — done when `cargo test combat::direct_vs_vehicle`。**先做欠账：录入 `result_vs_vehicle.json`（src:附1.5）+ 核对 1.2 尾值/1.4 矩阵**；对附1.2/1.4/1.5 各值做双人交叉核验后再编码。
- **T1.8** 〔🟡 Codex〕 直瞄通用流程（锁定/展开/冷却75s/射击条件/行进间仅坦克主炮） src:三.8 — done when `cargo test combat::direct_fire_flow`。
- **T1.9** 〔🟡 Codex〕 夺控 + 堆叠 src:三.4/三.5 — done when `cargo test rules::capture_and_stack`（夺控需周边6格无敌；堆叠第5个被拒）。
- **T1.10** 〔🔴 Opus〕 切片可玩闭环：`scenarios/demo_skirmish` 在脚本 bot 下跑完一局、产出可重放 replay dep:T1.1..T1.9 — done when `python tools/sim_smoke.py` 通过且 `make selfplay` 不崩。

## 阶段 2 — 火力与侦察（coverage M2，按序各一任务）
- **T2.1** 〔🟡 Codex〕 掩蔽细节（坦克射击中断转换；引导不退出/被引导退出） src:三.3 — done when `cargo test mechanics::concealment`.
- **T2.2** 〔🟡 Codex〕 上下车（75s + 压制中断） src:三.2 — done when `cargo test mechanics::mount_dismount`.
- **T2.3** 〔🔴 Opus〕 间瞄流水线 C（计划 Fly150/Boom300/CD300 + 校射三级 + 散布 + 命中/偏离结果 + 修正） dep:T1.5 src:三.9/附2 — done when `cargo test combat::indirect_fire`（验命中、散布n格、对己方误伤各一）。
- **T2.4** 〔🟡 Codex〕 巡飞弹（8s/格 +200m 侦察2 1200s 自毁 75s发射） src:三.10 — done when `cargo test units::loitering`.
- **T2.5** 〔🟡 Codex〕 无人机（8s/格 侦察2 地面仅相邻可见） src:三.11 — done when `cargo test units::uav`.
- **T2.6** 〔🟡 Codex〕 引导射击（步兵/无人车/无人机引导重型导弹 75s 准备） src:三.14 — done when `cargo test combat::guided_fire`.
- **T2.7** 〔🟡 Codex〕 侦察型战车50格 + 炮兵校射雷达开机75s（开机后炮火按格内校射） src:三.19 — done when `cargo test units::recon_and_radar`.
- **T2.8** 〔🔴 Opus〕 同格交战（距离0/首入即打/25s间隔/优先级/脱离惩罚） src:三.15 — done when `cargo test combat::same_hex`.

## 阶段 3 — 空中与特种（coverage M3，各一任务）
- **T3.1** 〔🟡 Codex〕 武装直升机（4s/格 对人10对车25 三高度修正） src:三.12 — done when `cargo test units::attack_heli`.
- **T3.2** 〔🟡 Codex〕 运输直升机（高/低/超低空 +500/200/20m 装卸75s 整体受击） src:三.16 — done when `cargo test units::transport_heli`.
- **T3.3** 〔🟡 Codex〕 无人战车（同战车 + 依托有人战车 + 可引导 + 母车毁则歼灭） src:三.13 — done when `cargo test units::ugv`.
- **T3.4** 〔🔴 Opus〕 防空流水线 D（攻击等级 + 歼灭阈值 + 多次裁决=车班数 + 各射速上限） src:三.18/附3 — done when `cargo test combat::air_defense`. **先对附3 车载尾值做双人交叉核验**。
- **T3.5** 〔🟡 Codex〕 聚合解聚（75s 总班≤4 弹药平均向下取整 4=2+2/3=2+1/2=1+1 炮兵不支持） src:三.17 — done when `cargo test rules::aggregate_split`.
- **T3.6** 〔🟡 Codex〕 行军（乡村40/一般60/等级90 沿路x2 转换75s 阻挡规则） src:三.1.3 — done when `cargo test mechanics::march`.

## 阶段 4 — 工事/雷场/支援（coverage M4，各一任务）
- **T4.1** 〔🟡 Codex〕 三类工事（容量5；12格可观察射击；隐蔽工事仅本格且不可射击；进出75s；超容退出） src:三.20 — done when `cargo test rules::fortifications`.
- **T4.2** 〔🟡 Codex〕 雷场流水线 E（损伤+装甲修正；布雷车50格×3 75s；扫雷/坦克半速开辟通路；通路单向） src:三.21/附4 — done when `cargo test rules::minefield`.
- **T4.3** 〔🟢 Haiku〕 天基侦察（不可机动/攻击；只显编号最小；不可直瞄） src:三.22 — done when `cargo test units::space_recon`.
- **T4.4** 〔🟡 Codex〕 间瞄炮火区进出裁决（设施） src:一.1.3 — done when `cargo test rules::barrage_zone`.

## 阶段 5 — 接口与产物（coverage M5）
- **T5.1** 〔🔴 Opus〕 LLM 工具接口对齐 schema + 迷雾观测序列化 dep:T1.5 — done when `python tools/validate.py prompts/examples/*.json` 通过 + `python python/examples/llm_agent.py --red scripted --blue scripted --dry-run` 跑通。
- **T5.2** 〔🔴 Opus〕 RL 真 Rust 后端替换 mock dep:T0.1 — done when `python -c "import openstratcore_env; ..."` 用 rust 后端跑 100 步不崩 + `make selfplay` 短训不发散。
- **T5.3** 〔🟡 Codex〕 Web 渲染真态势 + 真重放 dep:T0.2 — done when `cd web && npm run build` 通过且 ReplayViewer 能载入 replay JSON。
- **T5.4** 〔🔴 Opus〕 概率 provider 校准闭环 dep:T0.1 — done when `/calibrate-prob` 把某表从 static 切到 bayesian 后 `make test` 仍绿。
- **T5.5** 〔🟡 Codex〕 核心贴图出齐 — done when `assets/generated/manifest.json` 覆盖 coverage 列出的算子/地形且 `make validate` 绿。
- **T5.6** 〔🟡 Codex〕 地图编辑器（六角刷高程/地物/设施 + 导入 JSON/TMX + ajv 校验 + 导出） dep:T5.3 src:`schemas/map.schema.json` — done when `cd web && npm run build` 通过，且能导入 `scenarios/maps/demo_valley.map.json`→编辑→导出通过校验。UI 打磨参照 `frontend-design` skill，可委托 codex_gen。
- **T5.7** 〔🟡 Codex〕 想定编辑器（布阵/夺控/胜负 + 导入/校验/导出） dep:T5.6 src:`schemas/scenario.schema.json` — done when 能在所选地图上放置算子并导出通过 `schemas/scenario.schema.json` 校验。
- **T5.8** 〔🟡 Codex〕 规则编辑器（规则总表分组表单 + 18 张裁决表表格化编辑 + 导入/校验/导出） dep:T5.3 src:`schemas/rules.schema.json` — done when 能加载内置规则与任一 `config/tables/*`、编辑后导出通过校验；表标 `verified:false` 时给出需双人交叉核验的提示。
- **T5.9** 〔🔴 Opus〕 资源加载闭环：引擎能加载编辑器导出的"用户资源"跑一局 dep:T5.6,T5.7,T5.8 — done when 用导出的 user_map+user_scenario+user_rules 跑 `python tools/sim_smoke.py` 通过。

---

### 给自主模式的循环（精简版，详见 CLAUDE.md）
```
loop:
  读 docs/rules/coverage.md → 取第一个未勾选项
  在 TASKS.md 找对应 T 号 → 看 dep 是否都绿 → 看执行标签(🔴Opus/🟡Codex/🟢Haiku)定用谁
  若该项含数值：先对相关数值做双人交叉核验（两名独立作者各自录入并一致），再编码进 config
  按标签执行：🟡→codex_gen 外包；🟢→haiku 子代理；🔴→/model 升 Opus 亲自做 → make（对应测试）
  绿：codex_review → 修 → make verify-all → 勾 coverage → git commit
  红且自己修不动 / 规则有真歧义：停下并向人提问；否则继续 loop
```
