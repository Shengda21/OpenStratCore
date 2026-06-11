import { useEffect, useRef, useState } from "react";
import { Application, Rectangle } from "pixi.js";
import { HexRenderer, pixelToAxial, type SnapUnit } from "../render/HexRenderer";
import { initEngine, Engine } from "../engine";
import { loadSettings, type AppSettings } from "../settings";
import { llmDecide } from "../llm";

// A live match on the built-in combined-arms scenario — the SAME deterministic Rust kernel runs in the
// browser via wasm. Two modes: 观战 (both sides driven by a small in-browser scripted commander) and
// 我指挥红方 (you click to order the red units; the AI drives blue). Views can show the god view (全局)
// or either side's fog-of-war projection (rule #5).
const SCENARIO_URL = "/scenarios/rl_clash.scenario.json";
const CANVAS_W = 780;
const CANVAS_H = 460;
const HEX = 36;
const FIRE_HEXES = 3; // close enough to stop & engage instead of advancing
const HUMAN: Side = "red"; // the side you command in 我指挥 mode
const SYSTEM_PROMPT =
  "You are a tactical wargame commander in a real-time hex-grid land battle. Each tick you get a " +
  "fog-of-war observation (your units, visible enemies, the objective). Reply by calling the " +
  "submit_orders tool with an `actions` array. Ops: move_to {unitId,target:{q,r}}, " +
  "fire_direct {unitId,weapon,targetUnit}, stop {unitId}, capture {unitId}. A ground unit must be " +
  "stopped before it can fire. Win by holding the central control point or eliminating the enemy. " +
  "Be decisive: advance on the objective and concentrate fire. Output ONLY the tool call.";

type Side = "red" | "blue";
type ViewMode = "all" | Side;
type Mode = "watch" | "human";
interface ObsUnit {
  id: string; at?: { q: number; r: number }; type?: string; teams?: number;
  state?: string; weaponState?: string; busyUntil?: number;
}
interface MapHex { q: number; r: number; elevation: number; terrain: string; }
interface GameMap { hexes: MapHex[] }
interface Order { kind: "move" | "attack"; q?: number; r?: number; targetId?: string }

function hexDist(a: { q: number; r: number }, b: { q: number; r: number }): number {
  const dq = a.q - b.q, dr = a.r - b.r;
  return (Math.abs(dq) + Math.abs(dr) + Math.abs(dq + dr)) / 2;
}

interface Status { clock: number; owner: string | null; red: number; blue: number; redTeams: number; blueTeams: number; winner: string | null }
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
  // interaction refs (read by the once-attached pointer handler, so no stale closures)
  const modeRef = useRef<Mode>("watch");
  const selectedRef = useRef<string | null>(null);
  const ordersRef = useRef<Record<string, Order>>({});
  const renderRef = useRef<() => void>(() => {});
  const seedRef = useRef(1);
  const recordRef = useRef<{ commands: { t: number; side: Side; command: object }[]; snapshots: { t: number; state: unknown }[] }>({ commands: [], snapshots: [] });
  const tickCountRef = useRef(0);
  const savedRef = useRef(false);
  const schemaRef = useRef<object | null>(null);
  const metaRef = useRef<{ redName: string; blueName: string; mapFile: string; scenarioFile: string; rulesFile: string } | null>(null);

  const [ready, setReady] = useState(false);
  const [hasEngine, setHasEngine] = useState(false);
  const [view, setView] = useState<ViewMode>("all");
  const [mode, setMode] = useState<Mode>("watch");
  const [auto, setAuto] = useState(false);
  const [hint, setHint] = useState<string>("");
  const [status, setStatus] = useState<Status>(ZERO);
  const [error, setError] = useState<string | null>(null);
  const [llmNote, setLlmNote] = useState<string | null>(null);

  // Mount the PIXI canvas + renderer once; make the board clickable for 我指挥 mode.
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
      app.stage.eventMode = "static";
      app.stage.hitArea = new Rectangle(-5000, -5000, 10000, 10000);
      app.stage.on("pointertap", (e) => handleTap(e.global.x, e.global.y));
      await rendererRef.current.loadSprites();
      if (disposed) { app.destroy(true); return; }
      setReady(true);
    })();
    return () => { disposed = true; if (app) app.destroy(true); appRef.current = null; rendererRef.current = null; };
  }, []);

  function resetRecord() {
    recordRef.current = { commands: [], snapshots: [] };
    tickCountRef.current = 0; savedRef.current = false;
    const eng = engineRef.current;
    if (eng) recordRef.current.snapshots.push({ t: eng.clockSeconds(), state: JSON.parse(eng.snapshot()) });
  }

  async function buildEngine(seed: number) {
    const cfg = cfgRef.current; if (!cfg) return;
    ordersRef.current = {}; selectedRef.current = null; setHint(""); setLlmNote(null);
    seedRef.current = seed;
    engineRef.current = new Engine(cfg.map, cfg.scenario, cfg.rules, seed);
    resetRecord();
    render();
  }

  // Load the built-in scenario once PIXI is ready.
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    (async () => {
      try {
        const scenarioJson = await fetch(SCENARIO_URL).then((r) => { if (!r.ok) throw new Error(`${r.status} ${SCENARIO_URL}`); return r.text(); });
        const scenario = JSON.parse(scenarioJson) as { map: string; rules?: string; objectives?: { id: string; at: { q: number; r: number } }[] };
        const [mapJson, rulesJson, llmSchema] = await Promise.all([
          fetch(`/scenarios/maps/${scenario.map}`).then((r) => r.text()),
          fetch(`/config/${scenario.rules ?? "rules.default.json"}`).then((r) => r.text()),
          fetch(`/schemas/llm_tools.schema.json`).then((r) => (r.ok ? r.json() : null)).catch(() => null),
        ]);
        await initEngine();
        if (cancelled) return;
        const map = JSON.parse(mapJson) as GameMap;
        const rules = JSON.parse(rulesJson) as { timing?: { decision_tick_seconds?: number }; loadout?: Record<string, string[]> };
        const scn = JSON.parse(scenarioJson) as { sides?: { red?: { name?: string }; blue?: { name?: string } } };
        mapRef.current = map;
        loadoutRef.current = rules.loadout ?? {};
        stepRef.current = rules.timing?.decision_tick_seconds ?? 5;
        schemaRef.current = (llmSchema as { $defs?: { actionList?: object } } | null)?.$defs?.actionList ?? null;
        metaRef.current = {
          redName: scn.sides?.red?.name ?? "Red", blueName: scn.sides?.blue?.name ?? "Blue",
          mapFile: scenario.map, scenarioFile: SCENARIO_URL.split("/").pop() ?? "scenario.json",
          rulesFile: scenario.rules ?? "rules.default.json",
        };
        const obj = scenario.objectives?.[0];
        objRef.current = obj ? { id: obj.id, q: obj.at.q, r: obj.at.r } : null;
        cfgRef.current = { map: mapJson, scenario: scenarioJson, rules: rulesJson };
        const off = rendererRef.current!.centerOffset(map.hexes, CANVAS_W, CANVAS_H);
        appRef.current!.stage.position.set(off.x, off.y);
        seedRef.current = 1;
        engineRef.current = new Engine(mapJson, scenarioJson, rulesJson, 1);
        resetRecord();
        setHasEngine(true);
        render();
      } catch (e) { setError(`引擎初始化失败: ${(e as Error).message}`); }
    })();
    return () => { cancelled = true; };
  }, [ready]);

  function snapUnits(): (SnapUnit & { teams?: number })[] {
    const eng = engineRef.current; if (!eng) return [];
    const snap = JSON.parse(eng.snapshot()) as { units: Record<string, SnapUnit & { teams?: number }> };
    return Object.values(snap.units);
  }

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

  function render() {
    const eng = engineRef.current, r = rendererRef.current, map = mapRef.current, obj = objRef.current;
    if (!eng || !r || !map) return;
    let units: SnapUnit[];
    if (view === "all") units = snapUnits();
    else {
      const obs = JSON.parse(eng.observe(view)) as { ownUnits?: ObsUnit[]; enemyUnits?: ObsUnit[] };
      const other: Side = view === "red" ? "blue" : "red";
      const toSnap = (u: ObsUnit, s: Side): SnapUnit | null => (u && u.at ? { id: u.id, side: s, pos: u.at, unit_type: u.type, teams: u.teams } : null);
      units = [...(obs.ownUnits ?? []).map((u) => toSnap(u, view)), ...(obs.enemyUnits ?? []).map((u) => toSnap(u, other))].filter((u): u is SnapUnit => u !== null);
    }
    r.clear();
    r.drawMap(map);
    const st = computeStatus();
    if (obj) r.drawObjective({ q: obj.q, r: obj.r }, st.owner);
    if (modeRef.current === "human" && selectedRef.current) {
      const sel = snapUnits().find((u) => u.id === selectedRef.current && u.alive !== false);
      if (sel) r.drawSelection(sel.pos);
    }
    r.drawUnits(units);
    setStatus(st);
  }
  renderRef.current = render;

  // AI commander (used for both sides in 观战, and for blue in 我指挥).
  function aiCmd(u: ObsUnit, enemies: ObsUnit[], clock: number): object | null {
    const obj = objRef.current; if (!u.at || !obj) return null;
    if ((u.busyUntil ?? 0) > clock) return null;
    let near: ObsUnit | null = null, nd = Infinity;
    for (const e of enemies) { if (!e.at) continue; const d = hexDist(u.at, e.at); if (d < nd) { nd = d; near = e; } }
    const weapon = loadoutRef.current[u.type ?? ""]?.[0];
    if (near && near.at && nd <= FIRE_HEXES && weapon) {
      if (u.state !== "stopped") return { op: "stop", unitId: u.id };
      if ((u.weaponState ?? "deployed") === "deployed") return { op: "fire_direct", unitId: u.id, weapon, targetUnit: near.id };
      return null;
    }
    if (u.at.q === obj.q && u.at.r === obj.r) return { op: "capture", unitId: u.id };
    return { op: "move_to", unitId: u.id, target: { q: obj.q, r: obj.r } };
  }

  // Translate a player's standing order for one red unit into this tick's engine command.
  function humanCmd(u: ObsUnit, order: Order, enemies: ObsUnit[], clock: number): object | null {
    const obj = objRef.current; if (!u.at) return null;
    if ((u.busyUntil ?? 0) > clock) return null;
    const weapon = loadoutRef.current[u.type ?? ""]?.[0];
    if (order.kind === "attack") {
      const tgt = enemies.find((e) => e.id === order.targetId && e.at);
      if (tgt && tgt.at) {
        const d = hexDist(u.at, tgt.at);
        if (d <= 3 && weapon) {
          if (u.state !== "stopped") return { op: "stop", unitId: u.id };
          if ((u.weaponState ?? "deployed") === "deployed") return { op: "fire_direct", unitId: u.id, weapon, targetUnit: tgt.id };
          return null;
        }
        return { op: "move_to", unitId: u.id, target: { q: tgt.at.q, r: tgt.at.r } }; // close in
      }
      return null; // target no longer visible/alive — hold
    }
    // move order
    const tq = order.q ?? u.at.q, tr = order.r ?? u.at.r;
    if (u.at.q === tq && u.at.r === tr) {
      if (obj && tq === obj.q && tr === obj.r) return { op: "capture", unitId: u.id };
      return null; // arrived
    }
    return { op: "move_to", unitId: u.id, target: { q: tq, r: tr } };
  }

  // Who drives a side this tick: the human (in 我指挥, the HUMAN side), a configured LLM, or the
  // built-in scripted AI. The human side always wins a tie with the LLM config.
  function controllerFor(side: Side, s: AppSettings): "human" | "ai" | "llm" {
    if (modeRef.current === "human" && side === HUMAN) return "human";
    if (s.llm.enabled && s.llm.side === side) return "llm";
    return "ai";
  }

  // One 5s decision tick. May await an LLM call for an LLM-controlled side. No render — callers redraw.
  async function tickOnce() {
    const eng = engineRef.current; if (!eng) return;
    if (computeStatus().winner) return;
    const s = loadSettings();
    const t = eng.clockSeconds();
    const submit = (side: Side, cmd: object | null) => {
      if (!cmd) return;
      try { eng.submit(side, JSON.stringify(cmd), t); recordRef.current.commands.push({ t, side, command: cmd }); }
      catch { /* illegal / mid-transition = no-op (not recorded) */ }
    };
    for (const side of ["red", "blue"] as Side[]) {
      const ctrl = controllerFor(side, s);
      const o = JSON.parse(eng.observe(side)) as { ownUnits?: ObsUnit[]; enemyUnits?: ObsUnit[] };
      if (ctrl === "human") {
        for (const u of o.ownUnits ?? []) { const ord = ordersRef.current[u.id]; if (ord) submit(side, humanCmd(u, ord, o.enemyUnits ?? [], t)); }
      } else if (ctrl === "llm" && schemaRef.current) {
        try {
          const { actions } = await llmDecide(s.llm, side, o, schemaRef.current, SYSTEM_PROMPT);
          for (const a of actions) submit(side, a as object);
          setLlmNote(null);
        } catch (e) {
          setLlmNote(`LLM(${side}) 调用失败，本拍回退脚本 AI：${(e as Error).message}`);
          for (const u of o.ownUnits ?? []) submit(side, aiCmd(u, o.enemyUnits ?? [], t));
        }
      } else {
        for (const u of o.ownUnits ?? []) submit(side, aiCmd(u, o.enemyUnits ?? [], t));
      }
    }
    eng.step(stepRef.current);
    if (++tickCountRef.current % 4 === 0) recordRef.current.snapshots.push({ t: eng.clockSeconds(), state: JSON.parse(eng.snapshot()) });
  }

  // 前进: advance until something VISIBLE changes (a unit moves a hex, takes losses, or the match ends).
  // A single 5s tick is mid-move (a hex takes ~3-4 ticks), so one tick looks frozen; this guarantees
  // each click shows progress.
  async function advance() {
    const eng = engineRef.current; if (!eng || computeStatus().winner) return;
    const fingerprint = () => JSON.stringify(snapUnits().map((u) => [u.id, u.pos.q, u.pos.r, u.teams, u.alive]));
    const before = fingerprint();
    for (let i = 0; i < 12; i++) { await tickOnce(); if (fingerprint() !== before || computeStatus().winner) break; }
    render();
  }

  // Click on the board (我指挥 mode): pick your unit, or order the picked unit to move/attack.
  function handleTap(gx: number, gy: number) {
    const app = appRef.current, eng = engineRef.current;
    if (modeRef.current !== "human" || !app || !eng || computeStatus().winner) return;
    const local = app.stage.toLocal({ x: gx, y: gy });
    const hex = pixelToAxial(local.x, local.y, HEX);
    const here = snapUnits().find((u) => u.alive !== false && u.pos.q === hex.q && u.pos.r === hex.r);
    if (here && here.side === HUMAN) {
      selectedRef.current = here.id;
      setHint(`已选 ${here.id}：点空格→前往，点敌军→攻击`);
    } else if (selectedRef.current) {
      const sel = selectedRef.current;
      if (here && here.side !== HUMAN) { ordersRef.current[sel] = { kind: "attack", targetId: here.id }; setHint(`命令 ${sel} 攻击 ${here.id} → 点「前进」执行`); }
      else { ordersRef.current[sel] = { kind: "move", q: hex.q, r: hex.r }; setHint(`命令 ${sel} 前往 (${hex.q},${hex.r}) → 点「前进」执行`); }
    } else {
      setHint("先点一个你的单位（红圈）");
    }
    render();
  }

  useEffect(() => {
    if (!auto || !hasEngine) return;
    if (status.winner) { setAuto(false); return; }
    let busy = false, stopped = false;
    const h = setInterval(async () => {
      if (busy || stopped) return;            // skip if the previous tick (e.g. a slow LLM call) is still running
      busy = true;
      try { await tickOnce(); render(); } finally { busy = false; }
    }, 700);
    return () => { stopped = true; clearInterval(h); };
  }, [auto, hasEngine, status.winner, mode]);

  useEffect(() => { if (hasEngine) render(); /* eslint-disable-next-line */ }, [view]);

  // Build a schema-valid replay (header + command stream + periodic snapshots) and download it. The
  // Replay tab can load it back and scrub through the snapshots.
  function downloadReplay() {
    const eng = engineRef.current, meta = metaRef.current; if (!eng || !meta) return;
    const st = computeStatus();
    const rep = {
      header: {
        format: "openstratcore.replay", version: 1,
        redName: meta.redName, blueName: meta.blueName,
        mapFile: meta.mapFile, scenarioFile: meta.scenarioFile, rulesFile: meta.rulesFile,
        seed: seedRef.current, durationSeconds: eng.clockSeconds(),
        ...(st.winner === "red" || st.winner === "blue" ? { result: { winner: st.winner, reason: st.owner ? "capture" : "elimination" } } : {}),
      },
      commands: recordRef.current.commands,
      snapshots: recordRef.current.snapshots,
    };
    const blob = new Blob([JSON.stringify(rep, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url; a.download = `match-${seedRef.current}-${Math.round(eng.clockSeconds())}s.replay.json`;
    document.body.appendChild(a); a.click(); a.remove(); URL.revokeObjectURL(url);
  }

  // Auto-save a replay once a match ends (if enabled in Settings).
  useEffect(() => {
    if (status.winner && !savedRef.current) {
      savedRef.current = true;
      const eng = engineRef.current;
      if (eng) recordRef.current.snapshots.push({ t: eng.clockSeconds(), state: JSON.parse(eng.snapshot()) });
      if (loadSettings().autoSaveReplay) downloadReplay();
    }
  }, [status.winner]);

  function switchMode(m: Mode) {
    modeRef.current = m; setMode(m); setAuto(false);
    if (hasEngine) buildEngine(1); // fresh game for the chosen mode
  }
  function newMatch() { setAuto(false); buildEngine(Math.floor(performance.now()) % 100000); }

  const sideTag = (s: string | null) => (s === "red" ? "红方" : s === "blue" ? "蓝方" : "中立");
  const winBanner = status.winner ? `${status.winner === "red" ? "🟥 红方胜利" : "🟦 蓝方胜利"} — ${status.owner ? "夺控制胜" : "歼灭制胜"}` : null;

  return (
    <div style={{ marginTop: 16, display: "flex", gap: 20, flexWrap: "wrap" }}>
      <div>
        <h2 style={{ margin: "0 0 8px" }}>对局 · 实时六角格兵棋（浏览器内直跑 Rust 内核）</h2>
        {error && <pre style={{ color: "#ff9a9a", whiteSpace: "pre-wrap" }}>{error}</pre>}
        <div style={{ marginBottom: 8, display: "flex", gap: 8, alignItems: "center" }}>
          <span style={{ opacity: 0.8 }}>模式：</span>
          <button onClick={() => switchMode("watch")} aria-pressed={mode === "watch"} disabled={!hasEngine}>👁 观战 (AI 对 AI)</button>
          <button onClick={() => switchMode("human")} aria-pressed={mode === "human"} disabled={!hasEngine}>🎮 我指挥红方</button>
        </div>
        <div ref={hostRef} style={{ width: CANVAS_W, height: CANVAS_H, border: "1px solid #243042", borderRadius: 6, cursor: mode === "human" ? "pointer" : "default" }} />
        <div style={{ marginTop: 10, display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
          <button onClick={advance} disabled={!hasEngine || !!status.winner || auto}>前进 ▸</button>
          <button onClick={() => setAuto((a) => !a)} disabled={!hasEngine || !!status.winner}>{auto ? "⏸ 暂停" : "▶ 自动推进"}</button>
          <button onClick={newMatch} disabled={!hasEngine}>↻ 新对局</button>
          <button onClick={downloadReplay} disabled={!hasEngine} title="下载本局复盘 .replay.json（可在「复盘」标签载入）">💾 保存复盘</button>
          <span style={{ marginLeft: 8, opacity: 0.8 }}>视角：</span>
          <button onClick={() => setView("all")} aria-pressed={view === "all"} disabled={!hasEngine}>全局</button>
          <button onClick={() => setView("red")} aria-pressed={view === "red"} disabled={!hasEngine}>红方迷雾</button>
          <button onClick={() => setView("blue")} aria-pressed={view === "blue"} disabled={!hasEngine}>蓝方迷雾</button>
        </div>
        {llmNote && <div style={{ marginTop: 8, color: "#ffb86b" }}>{llmNote}</div>}
        {mode === "human" && <div style={{ marginTop: 8, minHeight: 20, color: "#ffe066" }}>{hint || "点你的单位（红圈）选中，再点格子下令；点「前进」执行一拍。"}</div>}
      </div>

      <aside style={{ width: 300, fontSize: 14, lineHeight: 1.6 }}>
        <div style={{ padding: "10px 12px", borderRadius: 6, marginBottom: 12, background: winBanner ? "#1d2b1d" : "#141c26", border: "1px solid #243042" }}>
          <div style={{ fontWeight: 700, marginBottom: 4 }}>战况</div>
          <div>时钟：<b>{status.clock.toFixed(0)}s</b></div>
          <div>控制点：<b style={{ color: status.owner === "red" ? "#ff8a8a" : status.owner === "blue" ? "#8ab0ff" : "#e2b53e" }}>{sideTag(status.owner)}</b></div>
          <div><span style={{ color: "#ff8a8a" }}>🟥 红方</span>：{status.red} 个单位 / {status.redTeams} 班·车</div>
          <div><span style={{ color: "#8ab0ff" }}>🟦 蓝方</span>：{status.blue} 个单位 / {status.blueTeams} 班·车</div>
          {winBanner && <div style={{ marginTop: 6, fontWeight: 700, fontSize: 16 }}>{winBanner}</div>}
        </div>

        <div style={{ padding: "10px 12px", borderRadius: 6, background: "#141c26", border: "1px solid #243042" }}>
          <div style={{ fontWeight: 700, marginBottom: 4 }}>怎么玩</div>
          {mode === "watch" ? (
            <ul style={{ margin: "4px 0 8px", paddingLeft: 18 }}>
              <li><b>▶ 自动推进</b>：红蓝双方均由内置 AI 指挥，自动打到分出胜负。</li>
              <li><b>前进 ▸</b>：手动推进到下一步可见变化（移动一格需若干秒，会自动跳到位）。</li>
              <li>想自己上手？切到 <b>🎮 我指挥红方</b>。</li>
            </ul>
          ) : (
            <ul style={{ margin: "4px 0 8px", paddingLeft: 18 }}>
              <li>① <b>点你的单位</b>（红圈）选中（高亮黄框）。</li>
              <li>② <b>点空格</b>=命令前往；<b>点敌军</b>=命令攻击（会自动停下开火）。</li>
              <li>③ <b>点「前进 ▸」</b>推进到下一步动作，或 <b>▶ 自动推进</b> 让命令自动执行；命令持续生效，直到你改令。</li>
              <li>⚠️ 不下令的单位会原地挨打——主动出击或抢控制点！</li>
            </ul>
          )}
          <div><b>目标</b>：抢占并控制中央<b>控制点</b>（金色六角），或歼灭对方。</div>
          <div style={{ fontWeight: 700, margin: "8px 0 4px" }}>图例</div>
          <div>🟥/🟦 圈 = 红/蓝单位（图标为兵种，括号内为班·车数）</div>
          <div>金色六角 = 控制点（描边色 = 当前控方）· 黄框 = 你选中的单位</div>
          <div>底色六角 = 地形（开阔/丛林/城镇/河流…）</div>
        </div>
      </aside>
    </div>
  );
}
