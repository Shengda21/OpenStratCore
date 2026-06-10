import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import { Application } from "pixi.js";
import { HexRenderer, type SnapUnit } from "../render/HexRenderer";
import { validateBySchema } from "../editor/resourceIO";

// Replay schema: schemas/replay.schema.json (the single contract — 硬规则#3). The viewer reuses the
// PIXI render layer to draw the real 态势 (unit positions/ownership) at the scrubbed time from the
// periodic `snapshots`. A replay with no snapshots can't be drawn without re-running the engine (wasm,
// ROADMAP P1); we say so rather than inventing a state.
const REPLAY_SCHEMA_URL = "/schemas/replay.schema.json";
const BUILTIN_REPLAY_URL = "/runs/demo_skirmish.replay.json";

interface SnapState {
  units?: Record<string, SnapUnit>;
  control?: Record<string, string | null>;
}
interface Replay {
  header: {
    redName?: string; blueName?: string; seed?: number; mapFile?: string; scenarioFile?: string;
  };
  commands: { t: number; side: string; command: unknown }[];
  snapshots?: { t: number; state: SnapState }[];
}

// Latest snapshot at-or-before `t` (binary search; the caller sorts snapshots by time on load). Before
// the first snapshot we have no recorded state, so show nothing rather than a later (future) frame.
function stateAtTime(snaps: { t: number; state: SnapState }[], t: number): SnapState | null {
  if (snaps.length === 0) return null;
  if (t < snaps[0].t) return null;
  let lo = 0, hi = snaps.length - 1, ans = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (snaps[mid].t <= t) { ans = mid; lo = mid + 1; } else { hi = mid - 1; }
  }
  return snaps[ans].state;
}

export function ReplayViewer() {
  const [replay, setReplay] = useState<Replay | null>(null);
  const [t, setT] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const appRef = useRef<Application | null>(null);
  const rendererRef = useRef<HexRenderer | null>(null);
  const [ready, setReady] = useState(false);

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
      await rendererRef.current.loadSprites(); // best-effort art pack; geometry fallback if absent
      if (disposed) { app.destroy(true); return; }
      setReady(true);
    })();
    return () => {
      disposed = true;
      if (app) app.destroy(true);
      appRef.current = null;
      rendererRef.current = null;
    };
  }, []);

  // Validate a candidate replay against the schema before accepting it (catches drift / bad files).
  async function load(raw: unknown) {
    const res = await validateBySchema(REPLAY_SCHEMA_URL, raw);
    if (!res.ok) {
      setReplay(null);
      setError(`不是合法的 replay（schemas/replay.schema.json）:\n` + res.errors.slice(0, 8).join("\n"));
      return;
    }
    const r = raw as Replay;
    // The schema doesn't enforce monotonic `t`; sort snapshots so the binary search + maxT hold even
    // for hand-edited / foreign replays.
    if (r.snapshots) r.snapshots = [...r.snapshots].sort((a, b) => a.t - b.t);
    setError(null);
    setReplay(r);
    setT(0);
  }

  async function onFile(e: ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      await load(JSON.parse(await file.text()));
    } catch (err) {
      setReplay(null);
      setError(`无法解析 JSON: ${(err as Error).message}`);
    }
  }

  async function loadBuiltin() {
    try {
      const raw = await fetch(BUILTIN_REPLAY_URL).then((r) => {
        if (!r.ok) throw new Error(`${r.status} ${BUILTIN_REPLAY_URL}`);
        return r.json();
      });
      await load(raw);
    } catch (err) {
      setReplay(null);
      setError(`载入内置复盘失败: ${(err as Error).message}`);
    }
  }

  const snaps = replay?.snapshots ?? [];
  const maxT = useMemo(() => {
    if (!replay) return 0;
    let m = 0;  // max over ALL entries (not .at(-1)) so it holds regardless of array ordering
    for (const c of replay.commands) m = Math.max(m, c.t);
    for (const s of snaps) m = Math.max(m, s.t);
    return m;
  }, [replay, snaps]);

  const stateAt = useMemo<SnapState | null>(() => stateAtTime(snaps, t), [snaps, t]);

  // Re-render the 态势 frame whenever the selected state (or readiness) changes.
  useEffect(() => {
    const r = rendererRef.current;
    if (!r || !ready) return;
    r.clear();
    if (!stateAt) return;
    // `state` is opaque per the replay schema; defensively keep only units carrying the fields the
    // renderer needs (id/side/pos) so a malformed or foreign snapshot degrades gracefully, not throws.
    const units = (Object.values(stateAt.units ?? {}) as Partial<SnapUnit>[])
      .filter((u): u is SnapUnit => !!u && !!u.pos && (u.side === "red" || u.side === "blue"));
    r.drawGridFor(units);
    r.drawUnits(units);
  }, [stateAt, ready]);

  return (
    <div style={{ marginTop: 16 }}>
      <h2>复盘查看器</h2>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <input type="file" accept="application/json" onChange={onFile} />
        <button onClick={loadBuiltin}>载入内置复盘 (demo)</button>
      </div>
      {error && (
        <pre style={{ marginTop: 8, color: "#ff9a9a", whiteSpace: "pre-wrap" }}>{error}</pre>
      )}
      <div
        ref={hostRef}
        style={{ marginTop: 12, width: 760, height: 440, border: "1px solid #243042" }}
      />
      {replay && (
        <div style={{ marginTop: 12 }}>
          <div className="hint">
            {replay.header.redName ?? "Red"} vs {replay.header.blueName ?? "Blue"} ·
            seed {replay.header.seed ?? 0} · {replay.commands.length} commands ·{" "}
            {snaps.length > 0 ? `${snaps.length} snapshots` : "无快照"}
          </div>
          {snaps.length > 0 ? (
            <>
              <input
                type="range"
                min={0}
                max={maxT}
                step="any"
                value={t}
                onChange={(e) => setT(Number(e.target.value))}
                style={{ width: 360 }}
              />
              <span style={{ marginLeft: 8 }}>t = {t.toFixed(1)}s</span>
            </>
          ) : (
            <div className="hint">
              此复盘只有指令流、未含快照——需先用 wasm 引擎重放生成态势（见 ROADMAP P1）。
            </div>
          )}
        </div>
      )}
    </div>
  );
}
