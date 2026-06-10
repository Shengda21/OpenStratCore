import { Application, Assets, Container, Graphics, Sprite, Text, Texture } from "pixi.js";

// Axial (q,r) pointy-top -> pixel. Keep in sync with crates/openstratcore-core/src/hex.rs.
const SQRT3 = Math.sqrt(3);

export function axialToPixel(q: number, r: number, size: number): { x: number; y: number } {
  const x = size * (SQRT3 * q + (SQRT3 / 2) * r);
  const y = size * (1.5 * r);
  return { x, y };
}

const TERRAIN_COLOR: Record<string, number> = {
  open: 0xcdbf94, forest: 0x4f7a4a, urban: 0x9aa0a6, river: 0x3f6fa3,
  lake: 0x2f5a8f, soft: 0xb08a55, road: 0xd8d2c2, rail: 0x8a8f96,
};

export interface MapHex { q: number; r: number; elevation: number; terrain: string; }
export interface GameMap { hexes: MapHex[]; }

/** A unit as carried in an engine snapshot's `state.units` (crates/openstratcore-core State serde). */
export interface SnapUnit {
  id: string;
  side: "red" | "blue";
  unit_type?: string;
  pos: { q: number; r: number };
  teams?: number;
  alive?: boolean;
  carried_by?: string | null;
}

const SIDE_COLOR: Record<string, number> = { red: 0xd64545, blue: 0x4573d6 };

interface ManifestEntry { group: string; file: string; }

/** Hex map renderer. Draws the flat military-electronic-map sprites from assets/generated/ when
 *  loaded (call `loadSprites()` once after construction), and degrades gracefully to geometric
 *  primitives (colored hexes + side-coloured discs) if the manifest/textures are unavailable —
 *  so the demo always renders, with or without the art pack. */
export class HexRenderer {
  app: Application;
  size: number;
  private tex = new Map<string, Texture>();

  constructor(app: Application, size = 28) {
    this.app = app;
    this.size = size;
  }

  /** Load the sprite atlas described by assets/generated/manifest.json into a key->Texture map.
   *  Idempotent and fully best-effort: any failure (no manifest, 404, decode error) leaves the
   *  renderer in geometry-fallback mode rather than throwing. Safe to await during view init. */
  async loadSprites(base = "/assets/generated"): Promise<void> {
    if (this.tex.size > 0) return;
    try {
      const manifest = (await fetch(`${base}/manifest.json`).then((r) => {
        if (!r.ok) throw new Error(`${r.status} manifest.json`);
        return r.json();
      })) as Record<string, ManifestEntry>;
      await Promise.all(
        Object.entries(manifest).map(async ([key, entry]) => {
          try {
            this.tex.set(key, await Assets.load(`${base}/${entry.file}`));
          } catch {
            /* skip a single missing/broken texture; the unit/hex falls back to geometry */
          }
        }),
      );
    } catch {
      this.tex.clear(); // manifest missing -> geometry everywhere
    }
  }

  drawMap(map: GameMap): void {
    // Terrain is drawn as crisp colored hex polygons (the hex_* art sprites carry an opaque neutral
    // background — `transparent:false` — so tiling them as quads bleeds at the corners). Flat color
    // tiles perfectly and reads as a clean tactical map; unit/objective art still use sprites.
    for (const h of map.hexes) {
      const { x, y } = axialToPixel(h.q, h.r, this.size);
      const g = new Graphics();
      const base = TERRAIN_COLOR[h.terrain] ?? 0x808080;
      const shade = Math.max(0, Math.min(40, h.elevation * 6));
      g.poly(hexPolygon(x, y, this.size)).fill({ color: darken(base, shade) }).stroke({ color: 0x10141a, width: 1 });
      this.app.stage.addChild(g);
    }
  }

  /** Mark an objective (control point) hex: the control_point sprite if loaded, else a star, plus a
   *  thick hex outline tinted by the current owner (neutral = amber, red/blue = side colour). Draw it
   *  AFTER the terrain and BEFORE the units so occupying units sit on top. */
  drawObjective(at: { q: number; r: number }, owner: string | null): void {
    const { x, y } = axialToPixel(at.q, at.r, this.size);
    const color = owner === "red" ? SIDE_COLOR.red : owner === "blue" ? SIDE_COLOR.blue : 0xe2b53e;
    const ring = new Graphics();
    ring.poly(hexPolygon(x, y, this.size * 0.94)).stroke({ color, width: 3 });
    this.app.stage.addChild(ring);
    const t = this.tex.get("control_point");
    if (t) {
      const sp = new Sprite(t);
      sp.anchor.set(0.5);
      sp.x = x;
      sp.y = y;
      sp.alpha = 0.85;
      sp.width = sp.height = this.size * 1.0;
      this.app.stage.addChild(sp);
    } else {
      const star = new Graphics();
      star.star(x, y, 5, this.size * 0.5, this.size * 0.22).fill({ color, alpha: 0.8 });
      this.app.stage.addChild(star);
    }
  }

  /** Center a given map's hex footprint inside a canvas of (w,h): returns the stage offset to apply
   *  via app.stage.position so the board sits centered rather than crammed in a corner. */
  centerOffset(hexes: { q: number; r: number }[], w: number, h: number): { x: number; y: number } {
    if (!hexes.length) return { x: w / 2, y: h / 2 };
    let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
    for (const hx of hexes) {
      const { x, y } = axialToPixel(hx.q, hx.r, this.size);
      minX = Math.min(minX, x); maxX = Math.max(maxX, x);
      minY = Math.min(minY, y); maxY = Math.max(maxY, y);
    }
    return { x: (w - (maxX + minX)) / 2, y: (h - (maxY + minY)) / 2 };
  }

  /** Clear everything so a frame can be re-rendered (replay scrubbing). Textures stay cached. */
  clear(): void {
    this.app.stage.removeChildren();
  }

  /** Draw a hex backdrop directly from a snapshot's unit footprint (no terrain map needed): a faint
   *  cell under each occupied hex, so 态势 reads as a grid even when only positions are known. */
  drawGridFor(units: SnapUnit[]): void {
    const seen = new Set<string>();
    for (const u of units) {
      const key = `${u.pos.q},${u.pos.r}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const { x, y } = axialToPixel(u.pos.q, u.pos.r, this.size);
      const g = new Graphics();
      g.poly(hexPolygon(x, y, this.size)).fill({ color: 0x1b2330 }).stroke({ color: 0x2c3a4d, width: 1 });
      this.app.stage.addChild(g);
    }
  }

  /** Render the live 态势: each alive, on-board unit as its APP-6-style sprite (or a side-coloured disc
   *  fallback), ringed in its side colour and labelled with id + 班/车数. A 被载 (carried) unit rides
   *  inside its carrier and is not drawn separately (rule #5). */
  drawUnits(units: SnapUnit[]): void {
    for (const u of units) {
      if (u.alive === false || (u.carried_by ?? null) !== null) continue;
      const { x, y } = axialToPixel(u.pos.q, u.pos.r, this.size);
      const c = new Container();
      const sideColor = SIDE_COLOR[u.side] ?? 0x888888;
      const t = this.tex.get(`${u.unit_type ?? ""}_${u.side}_force`);
      if (t) {
        // side-colour ring under the sprite so red/blue reads at a glance
        const ring = new Graphics();
        ring.circle(x, y, this.size * 0.62).fill({ color: sideColor, alpha: 0.18 })
          .stroke({ color: sideColor, width: 2 });
        c.addChild(ring);
        const sp = new Sprite(t);
        sp.anchor.set(0.5);
        sp.x = x;
        sp.y = y;
        sp.width = sp.height = this.size * 1.05;
        c.addChild(sp);
      } else {
        const dot = new Graphics();
        dot.circle(x, y, this.size * 0.42).fill({ color: sideColor }).stroke({ color: 0x0b0f14, width: 2 });
        c.addChild(dot);
      }
      const teams = u.teams ?? 0;
      const label = new Text({
        text: `${u.id}${teams ? ` (${teams})` : ""}`,
        style: { fill: 0xf2f4f8, fontSize: Math.max(9, this.size * 0.34), fontFamily: "monospace" },
      });
      label.x = x - label.width / 2;
      label.y = y + this.size * 0.45;
      c.addChild(label);
      this.app.stage.addChild(c);
    }
  }
}

function hexPolygon(x: number, y: number, size: number): number[] {
  const pts: number[] = [];
  for (let i = 0; i < 6; i++) {
    const ang = (Math.PI / 180) * (60 * i - 30);
    pts.push(x + size * Math.cos(ang), y + size * Math.sin(ang));
  }
  return pts;
}

function darken(color: number, amount: number): number {
  const r = Math.max(0, ((color >> 16) & 0xff) - amount);
  const gC = Math.max(0, ((color >> 8) & 0xff) - amount);
  const b = Math.max(0, (color & 0xff) - amount);
  return (r << 16) | (gC << 8) | b;
}
