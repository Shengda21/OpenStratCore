import { Application, Container, Graphics, Text } from "pixi.js";

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

/** Minimal hex map renderer. TODO(Day 7): unit sprites from assets/generated/manifest.json,
 *  fog-of-war shading, selection/move overlays, click-to-order wiring. */
export class HexRenderer {
  app: Application;
  size: number;

  constructor(app: Application, size = 28) {
    this.app = app;
    this.size = size;
  }

  drawMap(map: GameMap): void {
    for (const h of map.hexes) {
      const { x, y } = axialToPixel(h.q, h.r, this.size);
      const g = new Graphics();
      const pts: number[] = [];
      for (let i = 0; i < 6; i++) {
        const ang = (Math.PI / 180) * (60 * i - 30);
        pts.push(x + this.size * Math.cos(ang), y + this.size * Math.sin(ang));
      }
      const base = TERRAIN_COLOR[h.terrain] ?? 0x808080;
      const shade = Math.max(0, Math.min(40, h.elevation * 6));
      g.poly(pts).fill({ color: darken(base, shade) }).stroke({ color: 0x10141a, width: 1 });
      this.app.stage.addChild(g);
    }
  }

  /** Clear everything so a frame can be re-rendered (replay scrubbing). */
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
      const pts: number[] = [];
      for (let i = 0; i < 6; i++) {
        const ang = (Math.PI / 180) * (60 * i - 30);
        pts.push(x + this.size * Math.cos(ang), y + this.size * Math.sin(ang));
      }
      g.poly(pts).fill({ color: 0x1b2330 }).stroke({ color: 0x2c3a4d, width: 1 });
      this.app.stage.addChild(g);
    }
  }

  /** Render the live 态势: a side-coloured marker per alive, on-board unit, labelled with its id and
   *  班/车数. A 被载 (carried) unit rides inside its carrier and is not drawn separately (rule #5). */
  drawUnits(units: SnapUnit[]): void {
    for (const u of units) {
      if (u.alive === false || (u.carried_by ?? null) !== null) continue;
      const { x, y } = axialToPixel(u.pos.q, u.pos.r, this.size);
      const c = new Container();
      const dot = new Graphics();
      dot.circle(x, y, this.size * 0.42)
        .fill({ color: SIDE_COLOR[u.side] ?? 0x888888 })
        .stroke({ color: 0x0b0f14, width: 2 });
      c.addChild(dot);
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

function darken(color: number, amount: number): number {
  const r = Math.max(0, ((color >> 16) & 0xff) - amount);
  const gC = Math.max(0, ((color >> 8) & 0xff) - amount);
  const b = Math.max(0, (color & 0xff) - amount);
  return (r << 16) | (gC << 8) | b;
}
