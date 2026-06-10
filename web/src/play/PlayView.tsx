import { useEffect, useRef, useState } from "react";
import { Application } from "pixi.js";
import { HexRenderer, type SnapUnit } from "../render/HexRenderer";
import { initEngine, Engine } from "../engine";

// A live, watchable match on the built-in demo scenario: the SAME deterministic Rust kernel runs in the
// browser via wasm. Both sides are driven by a small in-browser scripted commander (advance to the
// objective; stop & fire when an enemy is close), so pressing 前进 actually plays a battle out to a
// winner. Rendering can show the god view (全局) or either side's fog-of-war projection (rule #5).
const SCENARIO_URL = "/scenarios/rl_duel.scenario.json";
const CANVAS_W = 780;
const CANVAS_H = 460;
const HEX = 36;
const FIRE_HEXES = 2; // close enough to stop & engage instead of advancing

type Side = "red" | "blue";
type ViewMode = "all" | Side;
interface ObsUnit {
  id: string; at?: { q: number; r: number }; type?: string; teams?: number;
  state?: string; weaponState?: string; busyUntil?: number;
}
interface MapHex { q: number; r: number; elevation: number; terrain: string; }
interface GameMap { hexes: MapHex[]; }

function hexDist(a: { q: number; r: number }, b: { q: number; r: number }): number {
  const dq = a.q - b.q, dr = a.r - b.r;
  return (Math.abs(dq) + Math.abs(dr) + Math.abs(dq + dr)) / 2;
}

interface Status { clock: number; owner: string | null; red: number; blue: number; redTeams: number; blueTeams: number; winner: string | null; }
const ZERO: Status = { clock: 0, owner: null, red: 0, blue: 0, redTeams: 0, blueTeams: 0, winner: null };

export function PlayView() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const appRef = useRef<Application | null>(null);
  const rendererRef = useRef<HexRenderer | null>(null);
  const engineRef = useRef<Engine | null>(null);
  const mapRef = useRef<GameMap | null>(null);
  const objRef = useRef<{ id: string; q: number; r: number } | null>(null);
  const loadoutRef = useRef<Record<string, string[]>>({});
  const cfgRef = useRef<{ map: string; scenario: string; rules: string } | null>(null);
  const stepRef = useRef(5);

  const [ready, setReady] = useState(false);
  const [hasEngine, setHasEngine] = useState(false);
  const [view, setView] = useState<ViewMode>("all");
  const [auto, setAuto] = useState(false);
  const [status, setStatus] = useState<Status>(ZERO);
  const [error, setError] = useState<string | null>(null);

  // Mount the PIXI canvas + renderer once (and load the sprite art pack).
  useEffect(() => {
    let app: Application | null = null;
    let disposed = false;
    (async () => {
      app = new Application();
      await app.init({ width: CANVAS_W, height: CANVAS_H, background: 0x0e141c, antialias: true });
      if (disposed) { app.destroy(true); return; }
      appRef.current = app;
      rendererRef.current = new HexRenderer(app, HEX);
      hostRef.current?.appendChild(app.canvas);
      await rendererRef.current.loadSprites();
      if (disposed) { app.destroy(true); return; }
      setReady(true);
    })();
    return () => { disposed = true; if (app) app.destroy(true); appRef.current = null; rendererRef.current = null; };
  }, []);

  // Build the engine from the built-in scenario once PIXI is ready.
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    (async () => {
      try {
        const scenarioJson = await fetch(SCENARIO_URL).then((r) => { if (!r.ok) throw new Error(`${r.status} ${SCENARIO_URL}`); return r.text(); });
        const scenario = JSON.parse(scenarioJson) as { map: string; rules?: string; objectives?: { id: string; at: { q: number; r: number } }[] };
        const [mapJson, rulesJson] = await Promise.all([
          fetch(`/scenarios/maps/${scenario.map}`).then((r) => r.text()),
          fetch(`/config/${scenario.rules ?? "rules.default.json"}`).then((r) => r.text()),
        ]);
        await initEngine();
        if (cancelled) return;
        const map = JSON.parse(mapJson) as GameMap;
        const rules = JSON.parse(rulesJson) as { timing?: { decision_tick_seconds?: number }; loadout?: Record<string, string[]> };
        mapRef.current = map;
        loadoutRef.current = rules.loadout ?? {};
        stepRef.current = rules.timing?.decision_tick_seconds ?? 5;
        const obj = scenario.objectives?.[0];
        objRef.current = obj ? { id: obj.id, q: obj.at.q, r: obj.at.r } : null;
        cfgRef.current = { map: mapJson, scenario: scenarioJson, rules: rulesJson };
        // center the board in the canvas
        const off = rendererRef.current!.centerOffset(map.hexes, CANVAS_W, CANVAS_H);
        appRef.current!.stage.position.set(off.x, off.y);
        engineRef.current = new Engine(mapJson, scenarioJson, rulesJson, 1);
        setHasEngine(true);
        render();
      } catch (e) {
        setError(`引擎初始化失败: ${(e as Error).message}`);
      }
    })();
    return () => { cancelled = true; };
  }, [ready]);

  function computeStatus(): Status {
    const eng = engineRef.current; if (!eng) return ZERO;
    const snap = JSON.parse(eng.snapshot()) as { units: Record<string, SnapUnit & { teams?: number }>; control?: Record<string, string | null> };
    const objId = objRef.current?.id;
    const owner = objId ? (snap.control?.[objId] ?? null) : null;
    let red = 0, blue = 0, redTeams = 0, blueTeams = 0;
    for (const u of Object.values(snap.units)) {
      if (u.alive === false) continue;
      if (u.side === "red") { red++; redTeams += u.teams ?? 0; }
      else if (u.side === "blue") { blue++; blueTeams += u.teams ?? 0; }
    }
    let winner: string | null = null;
    if (owner === "red" || owner === "blue") winner = owner;
    else if ((redTeams > 0) !== (blueTeams > 0)) winner = redTeams > 0 ? "red" : "blue";
    return { clock: eng.clockSeconds(), owner, red, blue, redTeams, blueTeams, winner };
  }

  // Draw the current frame for the selected view (god view = snapshot; side view = that side's fog-of-war).
  function render() {
    const eng = engineRef.current, r = rendererRef.current, map = mapRef.current, obj = objRef.current;
    if (!eng || !r || !map) return;
    let units: SnapUnit[];
    if (view === "all") {
      const snap = JSON.parse(eng.snapshot()) as { units: Record<string, SnapUnit> };
      units = Object.values(snap.units);
    } else {
      const obs = JSON.parse(eng.observe(view)) as { ownUnits?: ObsUnit[]; enemyUnits?: ObsUnit[] };
      const other: Side = view === "red" ? "blue" : "red";
      const toSnap = (u: ObsUnit, s: Side): SnapUnit | null => (u && u.at ? { id: u.id, side: s, pos: u.at, unit_type: u.type, teams: u.teams } : null);
      units = [
        ...(obs.ownUnits ?? []).map((u) => toSnap(u, view)),
        ...(obs.enemyUnits ?? []).map((u) => toSnap(u, other)),
      ].filter((u): u is SnapUnit => u !== null);
    }
    r.clear();
    r.drawMap(map);
    const st = computeStatus();
    if (obj) r.drawObjective({ q: obj.q, r: obj.r }, st.owner);
    r.drawUnits(units);
    setStatus(st);
  }

  // One scripted decision for a unit (from its side's fog-of-war view): fire a nearby enemy, else
  // advance to the objective (and seize it when standing on it).
  function decide(u: ObsUnit, enemies: ObsUnit[], clock: number): object | null {
    const obj = objRef.current; if (!u.at || !obj) return null;
    if ((u.busyUntil ?? 0) > clock) return null; // mid-transition; let it finish
    let near: ObsUnit | null = null, nd = Infinity;
    for (const e of enemies) { if (!e.at) continue; const d = hexDist(u.at, e.at); if (d < nd) { nd = d; near = e; } }
    const weapon = loadoutRef.current[u.type ?? ""]?.[0];
    if (near && near.at && nd <= FIRE_HEXES && weapon) {
      if (u.state !== "stopped") return { op: "stop", unitId: u.id };
      if ((u.weaponState ?? "deployed") === "deployed") return { op: "fire_direct", unitId: u.id, weapon, targetUnit: near.id };
      return null; // deploying
    }
    if (u.at.q === obj.q && u.at.r === obj.r) return { op: "capture", unitId: u.id };
    return { op: "move_to", unitId: u.id, target: { q: obj.q, r: obj.r } };
  }

  // Advance one decision tick: both sides issue scripted orders (from their own fog-of-war), then step.
  function stepOnce() {
    const eng = engineRef.current; if (!eng) return;
    if (computeStatus().winner) return;
    const t = eng.clockSeconds();
    for (const side of ["red", "blue"] as Side[]) {
      const obs = JSON.parse(eng.observe(side)) as { ownUnits?: ObsUnit[]; enemyUnits?: ObsUnit[] };
      for (const u of obs.ownUnits ?? []) {
        const cmd = decide(u, obs.enemyUnits ?? [], t);
        if (cmd) { try { eng.submit(side, JSON.stringify(cmd), t); } catch { /* illegal order = no-op */ } }
      }
    }
    eng.step(stepRef.current);
    render();
  }

  // Auto-play: tick every ~900ms until a winner is decided.
  useEffect(() => {
    if (!auto || !hasEngine) return;
    if (status.winner) { setAuto(false); return; }
    const h = setInterval(stepOnce, 900);
    return () => clearInterval(h);
  }, [auto, hasEngine, status.winner]);

  // Re-draw when the view (全局/红/蓝) changes.
  useEffect(() => { if (hasEngine) render(); /* eslint-disable-next-line */ }, [view]);

  function newMatch() {
    const cfg = cfgRef.current; if (!cfg) return;
    setAuto(false);
    const seed = Math.floor(performance.now()) % 100000; // a fresh game each time
    engineRef.current = new Engine(cfg.map, cfg.scenario, cfg.rules, seed);
    render();
  }

  const sideTag = (s: string | null) => (s === "red" ? "红方" : s === "blue" ? "蓝方" : "中立");
  const winBanner = status.winner
    ? `${status.winner === "red" ? "🟥 红方胜利" : "🟦 蓝方胜利"} — ${status.owner ? "夺控制胜" : "歼灭制胜"}`
    : null;

  return (
    <div style={{ marginTop: 16, display: "flex", gap: 20, flexWrap: "wrap" }}>
      <div>
        <h2 style={{ margin: "0 0 8px" }}>对局 · 实时六角格兵棋（浏览器内直跑 Rust 内核）</h2>
        {error && <pre style={{ color: "#ff9a9a", whiteSpace: "pre-wrap" }}>{error}</pre>}
        <div ref={hostRef} style={{ width: CANVAS_W, height: CANVAS_H, border: "1px solid #243042", borderRadius: 6 }} />
        <div style={{ marginTop: 10, display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
          <button onClick={stepOnce} disabled={!hasEngine || !!status.winner}>前进 {stepRef.current}s</button>
          <button onClick={() => setAuto((a) => !a)} disabled={!hasEngine || !!status.winner}>
            {auto ? "⏸ 暂停" : "▶ 自动推进"}
          </button>
          <button onClick={newMatch} disabled={!hasEngine}>↻ 新对局</button>
          <span style={{ marginLeft: 8, opacity: 0.8 }}>视角：</span>
          <button onClick={() => setView("all")} aria-pressed={view === "all"} disabled={!hasEngine}>全局</button>
          <button onClick={() => setView("red")} aria-pressed={view === "red"} disabled={!hasEngine}>红方迷雾</button>
          <button onClick={() => setView("blue")} aria-pressed={view === "blue"} disabled={!hasEngine}>蓝方迷雾</button>
        </div>
      </div>

      <aside style={{ width: 300, fontSize: 14, lineHeight: 1.6 }}>
        <div style={{
          padding: "10px 12px", borderRadius: 6, marginBottom: 12,
          background: winBanner ? "#1d2b1d" : "#141c26", border: "1px solid #243042",
        }}>
          <div style={{ fontWeight: 700, marginBottom: 4 }}>战况</div>
          <div>时钟：<b>{status.clock.toFixed(0)}s</b></div>
          <div>控制点：<b style={{ color: status.owner === "red" ? "#ff8a8a" : status.owner === "blue" ? "#8ab0ff" : "#e2b53e" }}>{sideTag(status.owner)}</b></div>
          <div><span style={{ color: "#ff8a8a" }}>🟥 红方</span>：{status.red} 个单位 / {status.redTeams} 班·车</div>
          <div><span style={{ color: "#8ab0ff" }}>🟦 蓝方</span>：{status.blue} 个单位 / {status.blueTeams} 班·车</div>
          {winBanner && <div style={{ marginTop: 6, fontWeight: 700, fontSize: 16 }}>{winBanner}</div>}
        </div>

        <div style={{ padding: "10px 12px", borderRadius: 6, background: "#141c26", border: "1px solid #243042" }}>
          <div style={{ fontWeight: 700, marginBottom: 4 }}>怎么玩</div>
          <ul style={{ margin: "4px 0 8px", paddingLeft: 18 }}>
            <li><b>目标</b>：抢占并控制中央<b>控制点</b>（金色六角），或歼灭对方。</li>
            <li><b>▶ 自动推进</b>：双方由内置 AI 指挥，一步步打到分出胜负。</li>
            <li><b>前进</b>：手动推进一个决策节拍（{stepRef.current}s，实时制·先到先裁）。</li>
            <li><b>视角</b>：全局看全场；红/蓝迷雾只显示该方按通视规则真正看得到的敌军。</li>
            <li><b>↻ 新对局</b>：换随机种子重开一局（同种子必复现）。</li>
          </ul>
          <div style={{ fontWeight: 700, margin: "8px 0 4px" }}>图例</div>
          <div>🟥/🟦 圈 = 红/蓝单位（图标为兵种，括号内为班·车数）</div>
          <div>金色六角 = 控制点（其描边颜色 = 当前控方）</div>
          <div>底色六角 = 地形（开阔/丛林/城镇/河流…）</div>
        </div>
      </aside>
    </div>
  );
}
