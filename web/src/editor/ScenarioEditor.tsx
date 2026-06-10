// ScenarioEditor.tsx — 图形化想定编辑器。在所选地图上布阵：阵营、算子（类型/班车数）、夺控点。
// 既可手动放置，也可从文本/JSON 导入。真源 schema: schemas/scenario.schema.json（硬规则#3）。
import { useEffect, useMemo, useState } from "react";
import { downloadJSON, loadBuiltin, parseTextToData, pickFile, validate } from "./resourceIO";
import { axialToPixel } from "../render/HexRenderer";

const UNIT_TYPES = ["tank", "infantry", "artillery", "ifv", "ugv", "uav", "attack_heli",
  "transport_heli", "loitering_munition", "recon_vehicle", "aa_gun", "aa_missile_squad",
  "aa_missile_vehicle", "minelayer", "minesweeper", "space_recon"];

interface SUnit { id: string; type: string; teams?: number; at: { q: number; r: number }; }
interface Side { name: string; units: SUnit[]; }
interface Scenario {
  format: string; version: number; name?: string; map?: string;
  sides: { red: Side; blue: Side };
  objectives?: { id: string; at: { q: number; r: number }; owner?: string; priority?: string }[];
}
interface Hex { q: number; r: number; terrain: string; elevation: number; }
interface GameMap { hexes: Hex[]; }

const SIZE = 22;
const TERRAIN_FILL: Record<string, string> = {
  open: "#cdbf94", urban: "#9aa0a6", forest: "#4f7a4a", river: "#3f6fa3",
  river_large: "#2f5a8f", lake: "#2f5a8f", soft: "#b08a55", road: "#d8d2c2", rail: "#8a8f96",
};

export function ScenarioEditor() {
  const [scn, setScn] = useState<Scenario | null>(null);
  const [map, setMap] = useState<GameMap | null>(null);
  const [side, setSide] = useState<"red" | "blue">("red");
  const [unitType, setUnitType] = useState(UNIT_TYPES[0]);
  const [count, setCount] = useState(4);
  const [errors, setErrors] = useState<string[]>([]);
  const [seq, setSeq] = useState(0);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    loadBuiltin("scenario").then((s) => setScn(s as Scenario)).catch(() => setScn(null));
    loadBuiltin("map").then((m) => setMap(m as GameMap)).catch(() => setMap(null));
  }, []);

  async function onImport() {
    const f = await pickFile(".json,.txt");
    if (!f) return;
    try {
      const data = parseTextToData("scenario", f.text, f.name);
      const v = await validate("scenario", data);
      setErrors(v.errors);
      if (v.ok) setScn(data as Scenario);
    } catch (e) {
      setErrors([String((e as Error).message ?? e)]);
    }
  }

  async function onExport() {
    if (!scn) return;
    const v = await validate("scenario", scn);
    setErrors(v.errors);
    if (v.ok) downloadJSON("user_scenario", scn);
  }

  function placeUnit(q: number, r: number) {
    if (!scn) return;
    const prefix = side === "red" ? "RE" : "BE";
    let n = seq;
    const ids = new Set([...scn.sides.red.units, ...scn.sides.blue.units].map((u) => u.id));
    let id = `${prefix}${n}`;
    while (ids.has(id)) id = `${prefix}${++n}`;
    scn.sides[side].units.push({ id, type: unitType, teams: count, at: { q, r } });
    setSeq(n + 1);
    setTick((t) => t + 1);
  }

  const units = useMemo(
    () => (scn ? [
      ...scn.sides.red.units.map((u) => ({ u, side: "red" as const })),
      ...scn.sides.blue.units.map((u) => ({ u, side: "blue" as const })),
    ] : []),
    [scn, tick],
  );

  const view = useMemo(() => {
    const pts = (map?.hexes ?? []).map((h) => axialToPixel(h.q, h.r, SIZE));
    const xs = pts.map((p) => p.x), ys = pts.map((p) => p.y);
    const minX = Math.min(0, ...xs) - SIZE * 2, minY = Math.min(0, ...ys) - SIZE * 2;
    const maxX = Math.max(0, ...xs) + SIZE * 2, maxY = Math.max(0, ...ys) + SIZE * 2;
    return { minX, minY, w: maxX - minX, h: maxY - minY };
  }, [map]);

  function hexPoints(cx: number, cy: number): string {
    const p: string[] = [];
    for (let i = 0; i < 6; i++) {
      const a = (Math.PI / 180) * (60 * i - 30);
      p.push(`${cx + SIZE * Math.cos(a)},${cy + SIZE * Math.sin(a)}`);
    }
    return p.join(" ");
  }

  const total = (scn?.sides.red.units.length ?? 0) + (scn?.sides.blue.units.length ?? 0);

  return (
    <div className="editor scenario-editor">
      <div className="toolbar">
        <strong>想定编辑器</strong>
        <button onClick={onImport}>导入文本/JSON</button>
        <button onClick={onExport}>导出为用户资源</button>
        <select value={side} onChange={(e) => setSide(e.target.value as "red" | "blue")}>
          <option value="red">红方</option><option value="blue">蓝方</option>
        </select>
        <select value={unitType} onChange={(e) => setUnitType(e.target.value)}>
          {UNIT_TYPES.map((t) => <option key={t}>{t}</option>)}
        </select>
        <label>班/车数 <input type="number" min={1} max={4} value={count}
          onChange={(e) => setCount(Math.min(4, Math.max(1, +e.target.value)))} /></label>
        <span className="hint">点击地图布阵 · {total} 个算子</span>
      </div>
      {errors.length > 0 && (
        <div className="errors">校验未过：<ul>{errors.map((e, i) => <li key={i}>{e}</li>)}</ul></div>
      )}
      {map && (
        <svg
          width={Math.min(820, view.w)}
          height={Math.min(520, view.h)}
          viewBox={`${view.minX} ${view.minY} ${view.w} ${view.h}`}
          style={{ background: "#0e141c", border: "1px solid #243042", marginTop: 8 }}
        >
          {map.hexes.map((h) => {
            const { x, y } = axialToPixel(h.q, h.r, SIZE);
            return (
              <polygon
                key={`${h.q},${h.r}`}
                points={hexPoints(x, y)}
                fill={TERRAIN_FILL[h.terrain] ?? "#666"}
                stroke="#10141a"
                strokeWidth={1}
                style={{ cursor: "crosshair" }}
                onClick={() => placeUnit(h.q, h.r)}
              />
            );
          })}
          {units.map(({ u, side: s }) => {
            const { x, y } = axialToPixel(u.at.q, u.at.r, SIZE);
            return (
              <g key={u.id} pointerEvents="none">
                <circle cx={x} cy={y} r={SIZE * 0.4}
                  fill={s === "red" ? "#d64545" : "#4573d6"} stroke="#0b0f14" strokeWidth={2} />
                <text x={x} y={y + 3} fontSize={9} fill="#f2f4f8" textAnchor="middle">{u.id}</text>
              </g>
            );
          })}
        </svg>
      )}
    </div>
  );
}
