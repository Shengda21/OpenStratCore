# AGENTS.md — Codex 工作规约（OpenStratCore）

## Codex 调用方式（Claude Code 这样调你）
```
codex exec --skip-git-repo-check "<任务prompt>" < /dev/null
```
非交互（别名 `codex e`）、已登录、模型 gpt-5.5；不要用交互式 TUI。非 git 目录必须加 `--skip-git-repo-check`，`< /dev/null` 防止卡在 stdin。末尾 `�ɹ�: ����ֹ PID...` 乱码是退出清理子进程的 GBK 提示，忽略。调用方超时设 ≥120000ms。


> `codex exec` / `codex review` 会读这份文件。它与 `CLAUDE.md` 保持一致；本文件聚焦 Codex 被调用时的行为。

## 你被调用的两种角色
1. **Reviewer（默认，只读）**：审 diff，找正确性/确定性/契约/边界问题。**只输出问题清单与修复建议，不直接改文件**，除非任务显式要求。优先级：先确定性与战争迷雾，再正确性，再可读性。
2. **Generator（显式授权时）**：实现边界清晰的机械任务。改动最小化，不动公共 API 签名（若必须，单列一节说明影响），产出可被 `make test`/`make validate` 验证。

## 必须遵守（与 CLAUDE.md 同源）
- **确定性**：随机只走 `openstratcore_core::rng::Rng`；禁 `thread_rng`、系统时间、依赖 HashMap 迭代顺序。
- **规则即数据**：数值改 `config/rules.*.json` + `schemas/rules.schema.json`，不硬编码。
- **契约先行**：跨层结构改动先改 `schemas/*.json`。
- **内核不 panic**：库代码返回 `Result`，禁 `unwrap/expect/panic`（测试除外）。
- **复盘**：不得破坏"录制→重放→状态一致"。

## 命令
- 构建 `cargo build --workspace`；测试 `cargo test --workspace`；静态检查 `cargo clippy --workspace --all-targets -- -D warnings`；格式 `cargo fmt --all`。
- 输出复审结论时若要求机器可读，用 `--json`/`--output-schema` 约定的结构。

## 输出约定
- Reviewer：Markdown 问题清单，每条含 `[严重度] 文件:行 — 问题 — 建议`。无问题就明说"无阻断性问题"。
- Generator：完成后给一段简短变更说明 + 自检结果（是否过 build/clippy/test）。

## 自主协作要点（与 CLAUDE.md 对齐）
- 规则真源在仓库内：`config/tables/*.json`（规则即数据；OpenStratCore 自有原创规则集，经双人交叉核验确立）。被 Claude Code 委托生成/复审时，凡涉及数值，**以仓库内 config 裁决表为准**，不要臆造。
- 进度真源：`docs/rules/coverage.md`；任务：`TASKS.md`。每个代码改动须满足 CLAUDE.md「完成定义」并能过 `make verify-all`。
- 复审输出务必逐条 `[严重度] 文件:行 — 问题 — 建议`；无阻断问题就明说。
