// resourceIO.ts — 资源模型 + 导入/校验/导出（地图/想定/规则 三类编辑器共用）
//
// 资源分两类（见 docs/RESOURCES.md）：
//   - 内置/默认资源（built-in）：随仓库发布，只读基线。规则=config/rules.default.json + config/tables/*；
//     想定/地图示例=scenarios/*。它们是内置规则与"为让程序跑起来"准备的最小集。
//   - 用户资源（user）：经图形化编辑器手动新建，或从文本/JSON 文件导入而来；可导出为 JSON 再分发。
//
// 校验：所有资源以 schemas/*.json 为唯一契约（硬规则#3）。导入与保存前都过 ajv。

// The repo's schemas declare `$schema: draft/2020-12`, so we MUST use the 2020 dialect of ajv — the
// default draft-07 `Ajv` throws "no schema with ref .../2020-12" when compiling them.
import Ajv2020 from "ajv/dist/2020";
import { type ValidateFunction } from "ajv";

export type ResourceKind = "map" | "scenario" | "rules";
export type Origin = "builtin" | "user";

export interface Resource<T = unknown> {
  kind: ResourceKind;
  origin: Origin;
  name: string;
  data: T;
}

// schema 路径（vite 下可用 ?url 或 fetch 加载；这里集中管理）
export const SCHEMA_PATH: Record<ResourceKind, string> = {
  map: "/schemas/map.schema.json",
  scenario: "/schemas/scenario.schema.json",
  rules: "/schemas/rules.schema.json",
};

const ajv = new Ajv2020({ allErrors: true, strict: false });
const validators: Partial<Record<ResourceKind, ValidateFunction>> = {};

export async function getValidator(kind: ResourceKind): Promise<ValidateFunction> {
  if (validators[kind]) return validators[kind]!;
  const schema = await fetch(SCHEMA_PATH[kind]).then((r) => r.json());
  const v = ajv.compile(schema);
  validators[kind] = v;
  return v;
}

export interface ValidationResult {
  ok: boolean;
  errors: string[];
}

export async function validate(kind: ResourceKind, data: unknown): Promise<ValidationResult> {
  const v = await getValidator(kind);
  const ok = v(data) as boolean;
  const errors = ok ? [] : (v.errors ?? []).map((e) => `${e.instancePath || "(root)"} ${e.message ?? ""}`);
  return { ok, errors };
}

// Validate arbitrary data against any schema URL (e.g. /schemas/replay.schema.json) using the shared
// ajv instance; compiled validators are cached by URL. For the three editable resources use
// validate(kind, …) above — this is for non-resource contracts like replays (硬规则#3).
const urlValidators: Record<string, ValidateFunction> = {};
export async function validateBySchema(schemaUrl: string, data: unknown): Promise<ValidationResult> {
  let v = urlValidators[schemaUrl];
  if (!v) {
    const schema = await fetch(schemaUrl).then((r) => r.json());
    v = ajv.compile(schema);
    urlValidators[schemaUrl] = v;
  }
  const ok = v(data) as boolean;
  const errors = ok ? [] : (v.errors ?? []).map((e) => `${e.instancePath || "(root)"} ${e.message ?? ""}`);
  return { ok, errors };
}

// 从文本（用户粘贴或读取的文件内容）解析为资源 JSON。支持 JSON；
// 预留 hook：TMX/CSV 等其它文本格式先转成本 schema 的 JSON（见 importText 的 TODO）。
export function parseTextToData(kind: ResourceKind, text: string, filename = ""): unknown {
  const trimmed = text.trim();
  if (filename.endsWith(".tmx") || trimmed.startsWith("<?xml")) {
    // TODO: 接 tools/tmx_import.py 的等价前端实现，将 Tiled TMX 转为 map.schema.json
    throw new Error("TMX 导入将走 tmx→map.schema 转换（见 tools/tmx_import.py），前端转换待 /editor 任务实现");
  }
  return JSON.parse(trimmed);
}

// 浏览器内"打开文件"→文本（编辑器从文本文件导入资源的入口）
export function pickFile(accept = ".json,.tmx,.txt"): Promise<{ name: string; text: string } | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.onchange = () => {
      const f = input.files?.[0];
      if (!f) return resolve(null);
      const reader = new FileReader();
      reader.onload = () => resolve({ name: f.name, text: String(reader.result ?? "") });
      reader.onerror = () => resolve(null);
      reader.readAsText(f);
    };
    input.click();
  });
}

// 导出资源为可下载的 JSON 文件（用户资源落盘的出口）
export function downloadJSON(name: string, data: unknown): void {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name.endsWith(".json") ? name : `${name}.json`;
  a.click();
  URL.revokeObjectURL(url);
}

// 加载内置默认资源（只读基线）。规则默认资源即 config/rules.default.json。
export async function loadBuiltin(kind: ResourceKind): Promise<unknown> {
  const path =
    kind === "rules"
      ? "/config/rules.default.json"
      : kind === "map"
        ? "/scenarios/maps/demo_valley.map.json"
        : "/scenarios/demo_skirmish.scenario.json";
  return fetch(path).then((r) => r.json());
}
