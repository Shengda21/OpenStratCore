# 资源模型（RESOURCES）

OpenStratCore 把**地图 / 想定 / 规则**都当作"资源"，分两层：

## 内置 / 默认资源（built-in，只读基线）
随仓库发布，是让程序能跑起来的最小可信集，**不在编辑器里改基线本体**（要改就另存为用户资源）：
- **规则**：`config/rules.default.json` + `config/tables/*.json`（OpenStratCore 自有的原创规则即数据；每张表均经双人交叉核验，真值以核验一致的数据本体为准）。
- **地图示例**：`scenarios/maps/demo_valley.map.json`。
- **想定示例**：`scenarios/demo_skirmish.scenario.json`。

## 用户资源（user，可创建/分发）
由图形化编辑器产出：
- **手动编辑**：在编辑器里直接画/填（地图刷高程地物设施、想定布阵、规则改表）。
- **从文本导入**：把符合 schema 的 JSON（或将来支持的 TMX/CSV 等）文件导入；编辑器解析→校验→载入。
- **导出**：校验通过后导出 JSON，可放进 `scenarios/`、`config/` 或单独分发。

## 三个编辑器（`web/src/editor/`）
| 编辑器 | 文件 | 产出 | 契约 schema |
|---|---|---|---|
| 地图 | `MapEditor.tsx` | `*.map.json` | `schemas/map.schema.json` |
| 想定 | `ScenarioEditor.tsx` | `*.scenario.json` | `schemas/scenario.schema.json` |
| 规则 | `RulesEditor.tsx` | `rules.*.json` + `config/tables/*` | `schemas/rules.schema.json`（+ 表形状约定） |

共用 `resourceIO.ts`：内置加载 / 文件导入 / **ajv 校验** / 导出下载。

## 导入与校验流水线
```
文本/JSON 文件 ──parseTextToData──▶ JS 对象 ──ajv(schema)──▶ {ok,errors}
   ok=true  ─▶ 载入编辑器 / 可导出 / 可被引擎加载
   ok=false ─▶ 在编辑器内列出每条 schema 错误，禁止导出
```
- 契约先行（硬规则#3）：字段以 `schemas/*.json` 为唯一真源；Rust serde、前端编辑器、`tools/validate.py` 三处共用同一份 schema。
- 规则数值落地前，仍须经双人交叉核验（两名独立作者各自录入同一格值并比对一致）（硬规则#2）。
- TMX 导入：先用 `tools/tmx_import.py` 的等价转换把 Tiled 地图转成 `map.schema.json`（前端转换由 `/editor` 任务实现）。

## 引擎如何消费资源
单一 Rust 内核（`openstratcore-core`）按 schema 反序列化任一资源；无论来自内置基线还是编辑器导出的用户资源，裁决完全一致（不存在"编辑器一套、引擎一套"的漂移）。
