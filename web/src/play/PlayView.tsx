import { useEffect, useRef, useState } from "react";
import { Application } from "pixi.js";
import { HexRenderer, type SnapUnit } from "../render/HexRenderer";
import { initEngine, Engine } from "../engine";

// Minimal live match: run the wasm kernel on the built-in demo scenario and render ONE side's
// fog-of-war 态势, advancing the clock by hand. Resources are served from the repo by the dev
// middleware (see vite.config.ts). Rendering goes through observe(side) — NOT the god-view snapshot()
// — so the view never shows un-observed enemy units (硬规则 #5).
const SCENARIO_URL = "/scenarios/demo_skirmish.scenario.json";
const SEED = 7;
type Side = "red" | "blue";

// Units as they appear in the fog-of-war observation (engine.rs observe(): `at`/`type`, enemy reduced).
interface ObsUnit { id: string; at?: { q: number; r: number }; type?: string; teams?: number; }
interface Observation { ownUnits?: ObsUnit[]; enemyUnits?: ObsUnit[]; }

export function PlayView() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const appRef = useRef<Application | null>(null);
  const rendererRef = useRef<HexRenderer | null>(null);
  const engineRef = useRef<Engine | null>(null);
  const [ready, setReady] = useState(false);
  const [hasEngine, setHasEngine] = useState(false);
  const [side, setSide] = useState<Side>("red");
  const [stepS, setStepS] = useState(5);
  const [clock, setClock] = useState(0);
  const [error, setError] = useState<string | null>(null);

  // Mount a PIXI canvas + HexRenderer once.
  useEffect(() => {
    let app: Application | null = null;
    let disposed = false;
    (async () => {
      app = new Application();
      await app.init({ width: 760, height: 440, background: 0x0e141c, antialias: true });
      if (disposed) { app.destroy(true); return; }
      app.stage.position.set(90, 90);
      appRef.current = app;
      rendererRef.current = new HexRenderer(app, 26);
      hostRef.current?.appendChild(app.canvas);
      setReady(true);
    })();
    return () => {
      disposed = true;
      if (app) app.destroy(true);
      appRef.current = null;
      rendererRef.current = null;
    };
  }, []);

  // Draw `viewerSide`'s fog-of-war view: own units + currently-observed enemies (both already filtered
  // by the engine). Map observe()'s `at`/`type` onto the renderer's SnapUnit; tag side per list.
  function render(viewerSide: Side) {
    const eng = engineRef.current, r = rendererRef.current;
    if (!eng || !r) return;
    const obs = JSON.parse(eng.observe(viewerSide)) as Observation;
    const enemySide: Side = viewerSide === "red" ? "blue" : "red";
    const toSnap = (u: ObsUnit, s: Side): SnapUnit | null =>
      u && u.at ? { id: u.id, side: s, pos: u.at, unit_type: u.type, teams: u.teams } : null;
    const units = [
      ...(obs.ownUnits ?? []).map((u) => toSnap(u, viewerSide)),
      ...(obs.enemyUnits ?? []).map((u) => toSnap(u, enemySide)),
    ].filter((u): u is SnapUnit => u !== null);
    r.clear();
    r.drawGridFor(units);
    r.drawUnits(units);
    setClock(eng.clockSeconds());
  }

  // Build the wasm engine from the built-in scenario once PIXI is ready.
  useEffect(() => {
    if (!ready) return;
    let cancelled = false;
    (async () => {
      try {
        const scenarioJson = await fetch(SCENARIO_URL).then((r) => {
          if (!r.ok) throw new Error(`${r.status} ${SCENARIO_URL}`);
          return r.text();
        });
        const scenario = JSON.parse(scenarioJson) as { map: string; rules?: string };
        const [mapJson, rulesJson] = await Promise.all([
          fetch(`/scenarios/maps/${scenario.map}`).then((r) => r.text()),
          fetch(`/config/${scenario.rules ?? "rules.default.json"}`).then((r) => r.text()),
        ]);
        await initEngine();
        if (cancelled) return;
        // The step increment is a rules tunable, not a hardcoded constant (规则即数据 #2).
        const rules = JSON.parse(rulesJson) as { timing?: { decision_tick_seconds?: number } };
        setStepS(rules.timing?.decision_tick_seconds ?? 5);
        engineRef.current = new Engine(mapJson, scenarioJson, rulesJson, SEED);
        setHasEngine(true);
        render(side);
      } catch (e) {
        setError(
          `引擎初始化失败: ${(e as Error).message}。需先 wasm-pack build 出 web/src/engine/pkg，` +
          `并从无空格本地路径起 vite（Z: UNC 路径含空格会破坏构建）。`,
        );
      }
    })();
    return () => { cancelled = true; };
  }, [ready]);

  // Re-draw when the viewer side changes (fog-of-war is per side).
  useEffect(() => { if (hasEngine) render(side); }, [side, hasEngine]);

  function stepOnce() {
    const eng = engineRef.current;
    if (!eng) return;
    eng.step(stepS);
    render(side);
  }

  return (
    <div style={{ marginTop: 16 }}>
      <h2>对局（wasm 直跑内核 · {side === "red" ? "红方" : "蓝方"}视角）</h2>
      {error && <pre style={{ color: "#ff9a9a", whiteSpace: "pre-wrap" }}>{error}</pre>}
      <div
        ref={hostRef}
        style={{ marginTop: 12, width: 760, height: 440, border: "1px solid #243042" }}
      />
      <div style={{ marginTop: 12, display: "flex", gap: 8, alignItems: "center" }}>
        <button onClick={stepOnce} disabled={!hasEngine}>前进 {stepS}s</button>
        <button onClick={() => setSide("red")} aria-pressed={side === "red"} disabled={!hasEngine}>红方视角</button>
        <button onClick={() => setSide("blue")} aria-pressed={side === "blue"} disabled={!hasEngine}>蓝方视角</button>
        <span className="hint">demo_skirmish · seed {SEED} · clock = {clock.toFixed(0)}s</span>
      </div>
    </div>
  );
}
