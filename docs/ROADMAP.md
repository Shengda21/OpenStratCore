# ROADMAP — 第一周（纵向切片优先）

目标：跑通"六角地图 + 坦克&步兵 + 机动状态机 + 直瞄射击 + 夺控胜负"的最小可玩闭环，
并让 RL 自博弈骨架（mock 后端）与 LLM 指挥官（API）都能各打一局、能录制+重放。

## Day 1 — 契约与骨架
- 定稿 5 套 schema（map/scenario/rules/replay/llm_tools）+ `config/rules.default.json` 的切片子集。
- `make validate` 绿；Rust 工作区 `cargo build` 通过（mechanic 用 `todo!()` 占位）。
- 门禁：`/codex-review` 过一遍骨架。

## Day 2 — 六角地图 + 状态 + 事件队列
- `hex.rs`（坐标/距离/邻接）、`types.rs`（map/scenario/unit/state）、`engine.rs`（事件队列 + 决策 tick）落地。
- 加载 `scenarios/demo_skirmish` 成功；`advance_to_next_event` / `step(dt)` 可推进空局。
- 测试：地图加载、距离计算、空局推进确定性。

## Day 3 — 机动（规则 1）
- `/add-rule 1 机动`：车辆/人员速度、地形/坡度修正、机动↔停止 75s 转换、压制打断。
- 测试：固定 seed 下到达时间、坡度>5 不可通行、被压制转被压制态。

## Day 4 — 通视/观察（规则 6/7）
- `/add-rule 6 通视` + `/add-rule 7 观察`：LOS 线性插值 + 高地物遮挡 + 各兵种观察距离表 + 掩蔽减半。
- 战争迷雾观测成形（`Engine::observe(side)` 只给可见信息）。
- 测试：通视遮挡用例、观察距离边界、迷雾不泄漏。

## Day 5 — 直瞄射击（规则 8）
- `/add-rule 8 直瞄`：武器展开 75s/冷却 75s、射击条件、攻击等级查表→`ProbProvider`→战果、高度差修正、战损修正。
- `prob` 先接 `static` + 预留 `bayesian`。
- 测试：固定 seed 命中/压制/毁伤、修正叠加正确。

## Day 6 — 夺控 + 闭环
- `/add-rule 5 夺控`：到夺控点中心 + 周边无敌可夺控；胜负与时限判定。
- 脚本对手 + `/selfplay --backend mock` 冒烟；`/llm-match --red anthropic --blue scripted` 打一局。
- 复盘录制 + `/replay-verify` 绿。

## Day 7 — 前端最小可视 + 巩固
- `web/` 用 wasm 加载内核，渲染地图/单位、跑一局、复盘查看器拖时间轴（骨架→可用）。
- 出第一批贴图：`/gen-art terrain` + `/gen-art units`。
- 全量 `make test && make lint && make validate` 绿；写下"规则覆盖清单"现状。

## 之后（按模块逐条 /add-rule）
间瞄(9) → 空中(10/11/12) → 无人战车(13)/引导(14)/同格(15) → 运输(16)/聚合解聚(17) → 防空(18)/侦察校射(19) → 工事(20)/雷场(21)/天基(22)。每条都走完整 `/add-rule` 门禁。
