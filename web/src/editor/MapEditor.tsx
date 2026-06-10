// MapEditor.tsx — 图形化地图编辑器。
// 真源 schema: schemas/map.schema.json。导入与导出都经 resourceIO.validate("map", …)（硬规则#3）。
// 设施 (设施/facilities) are scenario-level (schemas/scenario.schema.json), not a map-hex field, so
// the map brush edits the two per-hex map fields: 高程 (elevation) and 地物 (terrain).
import { useEffect, useMemo, useState } from "react";
import { downloadJSON, loadBuiltin, parseTextToData, pickFile, validate } from "./resourceIO";
import { axialToPixel } from "../render/HexRenderer";

type Brush = "elevation" | "terrain";
// Must match schemas/map.schema.json $defs/hex.terrain.enum exactly.
const TERRAINS = ["open", "urban", "forest", "river", "river_large", "lake", "soft", "road", "rail"];
const TERRAIN_FILL: Record<string, string> = {
  open: "#cdbf94", urban: "#9aa0a6", forest: "#4f7a4a", river: "#3f6fa3",
  river_large: "#2f5a8f", lake: "#2f5a8f", soft: "#b08a55", road: "#d8d2c2", rail: "#8a8f96",
};

interface Hex { q: number; r: number; id?: string; elevation: number; terrain: string; road?: unknown; }
interface GameMap {
  format: string; version: number; name?: string; elevationUnitMeters?: number; hexes: Hex[];
}

const SIZE = 22;

export function MapEditor() {
  const [map, setMap] = useState<GameMap | null>(null);
  const [brush, setBrush] = useState<Brush>("terrain");
  const [terrain, setTerrain] = useState(TERRAINS[2]);
  const [elev, setElev] = useState(1);
  const [errors, setErrors] = useState<string[]>([]);
  const [tick, setTick] = useState(0); // force re-render after in-place hex edits

  useEffect(() => {
    loadBuiltin("map")
      .then((m) => setMap(m as GameMap))
      .catch(() => setMap({ format: "openstratcore.map", version: 1, name: "untitled",
        elevationUnitMeters: 10, hexes: [] }));
  }, []);

  async function onImport() {
    const f = await pickFile(".json,.tmx,.txt");
    if (!f) return;
    try {
      const data = parseTextToData("map", f.text, f.name);
      const v = await validate("map", data);
      setErrors(v.errors);
      if (v.ok) setMap(data as GameMap);
    } catch (e) {
      setErrors([String((e as Error).message ?? e)]);
    }
  }

  async function onExport() {
    if (!map) return;
    const v = await validate("map", map);
    setErrors(v.errors);
    if (v.ok) downloadJSON("user_map", map);
  }

  function paintHex(h: Hex) {
    if (brush === "terrain") h.terrain = terrain;
    else h.elevation = Math.max(0, elev); // schema: elevation >= 0
    setTick((n) => n + 1);
  }

  // SVG geometry: project every hex, fit a viewBox around them.
  const view = useMemo(() => {
    const pts = (map?.hexes ?? []).map((h) => axialToPixel(h.q, h.r, SIZE));
    const xs = pts.map((p) => p.x), ys = pts.map((p) => p.y);
    const minX = Math.min(0, ...xs) - SIZE * 2, minY = Math.min(0, ...ys) - SIZE * 2;
    const maxX = Math.max(0, ...xs) + SIZE * 2, maxY = Math.max(0, ...ys) + SIZE * 2;
    return { minX, minY, w: maxX - minX, h: maxY - minY };
  }, [map, tick]);

  function hexPoints(cx: number, cy: number): string {
    const p: string[] = [];
    for (let i = 0; i < 6; i++) {
      const a = (Math.PI / 180) * (60 * i - 30);
      p.push(`${cx + SIZE * Math.cos(a)},${cy + SIZE * Math.sin(a)}`);
    }
    return p.join(" ");
  }

  return (
    <div className="editor map-editor">
      <div className="toolbar">
        <strong>地图编辑器</strong>
        <button onClick={onImport}>导入文本/JSON/TMX</button>
        <button onClick={onExport}>导出为用户资源</button>
        <span>笔刷：</span>
        <select value={brush} onChange={(e) => setBrush(e.target.value as Brush)}>
          <option value="elevation">高程</option>
          <option value="terrain">地物</option>
        </select>
        {brush === "terrain" && (
          <select value={terrain} onChange={(e) => setTerrain(e.target.value)}>
            {TERRAINS.map((t) => <option key={t}>{t}</option>)}
          </select>
        )}
        {brush === "elevation" && (
          <input type="number" min={0} value={elev} onChange={(e) => setElev(+e.target.value)} />
        )}
        <span className="hint">点击六角格落笔 · {map?.name ?? "?"} · {map?.hexes?.length ?? 0} 格</span>
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
              <g key={`${h.q},${h.r}`} onClick={() => paintHex(h)} style={{ cursor: "pointer" }}>
                <polygon
                  points={hexPoints(x, y)}
                  fill={TERRAIN_FILL[h.terrain] ?? "#666"}
                  stroke="#10141a"
                  strokeWidth={1}
                />
                <text x={x} y={y + 4} fontSize={11} fill="#10141a" textAnchor="middle">
                  {h.elevation}
                </text>
              </g>
            );
          })}
        </svg>
      )}
    </div>
  );
}
