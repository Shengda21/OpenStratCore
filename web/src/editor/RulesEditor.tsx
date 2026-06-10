// RulesEditor.tsx — 图形化规则编辑器（骨架）
// 在内置规则（config/rules.default.json + config/tables/*）基础上增改：时长、速度、射程、
// 各裁决表与修正、概率 provider 配置。既可手动编辑，也可从文本/JSON 导入整套规则或单张表。
// 真源契约: schemas/rules.schema.json（裁决表另有 config/tables 的形状约定）。
// 重要：任何数值的权威真值仍是 config/tables/*.json —— 编辑器只是让人/agent 更方便地产出与校验规则资源。
import { useEffect, useState } from "react";
import { downloadJSON, loadBuiltin, parseTextToData, pickFile, validate } from "./resourceIO";

// Editable scalar groups of the rules config (规则总表): each numeric leaf becomes an input. The
// authoritative source of every value lives in config/tables/*.json — this just makes producing/validating a
// rules resource convenient (the export re-validates against schemas/rules.schema.json).
const SCALAR_GROUPS = ["timing", "control", "fortification", "minefield", "air"] as const;

/** A flat editable number-table for a `rules[group]` object of scalars (skips nested objects). */
function ScalarTable({ rules, group, onChange }: {
  rules: Record<string, unknown>; group: string; onChange: () => void;
}) {
  const obj = (rules?.[group] ?? {}) as Record<string, unknown>;
  const keys = Object.keys(obj).filter((k) => typeof obj[k] === "number");
  if (keys.length === 0) return null;
  return (
    <div className="scalar-group">
      <h4>{group}</h4>
      <table className="scalar-table">
        <tbody>
          {keys.map((k) => (
            <tr key={k}>
              <td>{k}</td>
              <td>
                <input type="number" value={obj[k] as number}
                  onChange={(e) => { obj[k] = Number(e.target.value); onChange(); }} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// 与 config/tables/ 对应的可编辑表清单（编辑器按此渲染表格视图）
const TABLE_FILES = [
  "observation_distance", "unit_speed", "terrain_movement",
  "attack_level_vs_personnel", "attack_level_vs_vehicle", "attack_level_infantry_vs_vehicle",
  "height_diff_correction", "result_vs_vehicle", "vehicle_loss_correction",
  "result_vs_personnel", "personnel_loss_correction",
  "indirect_scatter", "indirect_result", "indirect_correction",
  "aa_attack_level", "aa_result", "minefield", "timings",
];

export function RulesEditor() {
  const [rules, setRules] = useState<any>(null);
  const [tab, setTab] = useState<"rules" | "tables">("rules");
  const [tableName, setTableName] = useState(TABLE_FILES[0]);
  const [table, setTable] = useState<any>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [, setTick] = useState(0); // re-render after in-place scalar edits

  useEffect(() => { loadBuiltin("rules").then(setRules).catch(() => setRules({ format: "openstratcore.rules", version: 1 })); }, []);
  useEffect(() => { fetch(`/config/tables/${tableName}.json`).then((r) => r.json()).then(setTable).catch(() => setTable(null)); }, [tableName]);

  async function onImportRules() {
    const f = await pickFile(".json,.txt");
    if (!f) return;
    try {
      const data = parseTextToData("rules", f.text, f.name);
      const v = await validate("rules", data); setErrors(v.errors);
      if (v.ok) setRules(data);
    } catch (e: any) { setErrors([String(e.message ?? e)]); }
  }
  async function onExportRules() {
    const v = await validate("rules", rules); setErrors(v.errors);
    if (v.ok) downloadJSON("user_rules", rules);
  }
  async function onImportTable() {
    const f = await pickFile(".json,.txt");
    if (!f) return;
    try { setTable(parseTextToData("rules", f.text, f.name)); setErrors([]); }
    catch (e: any) { setErrors([String(e.message ?? e)]); }
  }
  function onExportTable() { if (table) downloadJSON(`${table.id ?? tableName}`, table); }

  return (
    <div className="editor rules-editor">
      <div className="toolbar">
        <strong>规则编辑器</strong>
        <button onClick={() => setTab("rules")} aria-pressed={tab === "rules"}>规则总表</button>
        <button onClick={() => setTab("tables")} aria-pressed={tab === "tables"}>裁决表</button>
      </div>

      {tab === "rules" && (
        <div>
          <div className="toolbar"><button onClick={onImportRules}>导入规则文本/JSON</button><button onClick={onExportRules}>导出为用户规则</button></div>
          {rules && (
            <div className="scalar-groups">
              {SCALAR_GROUPS.map((g) => (
                <ScalarTable key={g} rules={rules} group={g} onChange={() => setTick((t) => t + 1)} />
              ))}
            </div>
          )}
          <pre className="json-view">{rules ? JSON.stringify(rules, null, 2).slice(0, 2000) : "加载中…"}</pre>
        </div>
      )}

      {tab === "tables" && (
        <div>
          <div className="toolbar">
            <select value={tableName} onChange={(e) => setTableName(e.target.value)}>{TABLE_FILES.map((t) => <option key={t}>{t}</option>)}</select>
            <button onClick={onImportTable}>导入此表文本/JSON</button>
            <button onClick={onExportTable}>导出此表</button>
            {table && table.verified === false && <span className="warn">⚠️ 此表标注未核对，落地前请双人交叉核验（参见 附录 p{table?.source?.page}）</span>}
          </div>
          {/* TODO /editor: 把矩阵/字典渲染成可编辑表格（行=随机数/距离/班数，列=…）*/}
          <pre className="json-view">{table ? JSON.stringify(table, null, 2) : "（无此表）"}</pre>
        </div>
      )}

      {errors.length > 0 && <div className="errors">校验未过：<ul>{errors.map((e, i) => <li key={i}>{e}</li>)}</ul></div>}
    </div>
  );
}
