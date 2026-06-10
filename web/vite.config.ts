import { defineConfig, type Plugin, type ResolvedConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve, sep, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createReadStream, existsSync, statSync, cpSync } from "node:fs";

// The wasm engine (crates/openstratcore-wasm) is built with wasm-pack into web/src/engine/pkg
// and imported by the render layer. The pkg is gitignored (built by CI, shipped via Release).
//
// BUILD NOTE: `npm run build` must run from a path WITHOUT spaces in its resolved root. If this repo
// lives on a mapped drive whose UNC target has a space (e.g. Z: -> \\host\Shared Folders\...),
// rollup mangles absolute paths and the build fails (`tsc` still passes). Build from a local copy /
// no-space junction in that case. The code itself is build-clean.

// The app fetches canonical repo data at runtime: /schemas (editor validation), /config (rules +
// 18 decision tables), /scenarios (+ maps), and /assets (unit/terrain art). This plugin serves those
// straight from the repo during `vite dev` AND copies them into dist/ on `vite build`, so a static
// deploy is fully self-contained — no server middleware needed. (The demo replay ships separately via
// web/public/runs/, which Vite copies to dist/runs/ verbatim.)
function repoAssets(): Plugin {
  const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
  const DIRS = ["schemas", "config", "scenarios", "assets"];
  const BASES = DIRS.map((d) => resolve(ROOT, d) + sep);
  const TYPES: Record<string, string> = {
    ".json": "application/json",
    ".png": "image/png",
    ".svg": "image/svg+xml",
    ".wasm": "application/wasm",
  };
  let outDir = "";
  return {
    name: "repo-assets",
    configResolved(c: ResolvedConfig) {
      outDir = resolve(c.root, c.build.outDir);
    },
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = (req.url ?? "").split("?")[0];
        if (!DIRS.some((d) => url.startsWith(`/${d}/`))) return next();
        const fp = resolve(ROOT, "." + decodeURIComponent(url));
        // Traversal guard: the resolved file must stay INSIDE one of the whitelisted asset dirs (so a
        // `/schemas/../package.json` cannot reach other repo files) and be a real file.
        if (!BASES.some((b) => fp.startsWith(b)) || !existsSync(fp) || !statSync(fp).isFile()) {
          return next();
        }
        const ext = fp.slice(fp.lastIndexOf("."));
        res.setHeader("Content-Type", TYPES[ext] ?? "application/octet-stream");
        createReadStream(fp).pipe(res);
      });
    },
    closeBundle() {
      // After the bundle is written, copy the canonical repo dirs into dist/ so the static build
      // resolves /schemas, /config, /scenarios, /assets with no server middleware.
      for (const d of DIRS) {
        const src = resolve(ROOT, d);
        if (existsSync(src)) cpSync(src, join(outDir, d), { recursive: true });
      }
    },
  };
}

export default defineConfig({
  plugins: [react(), repoAssets()],
  server: { port: 5173 },
});
