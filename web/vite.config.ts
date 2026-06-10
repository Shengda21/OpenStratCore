import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { createReadStream, existsSync, statSync } from "node:fs";

// The wasm engine (crates/openstratcore-wasm) is built with wasm-pack into web/src/engine/pkg
// and imported by the render layer once available.
//
// BUILD NOTE: `npm run build` must run from a path WITHOUT spaces in its resolved root. If this repo
// lives on a mapped drive whose UNC target has a space (e.g. Z: -> \\host\Shared Folders\...),
// rollup mangles absolute paths and the build fails (`tsc` still passes). Build from a local copy /
// no-space junction in that case. The code itself is build-clean.

// Dev-only: serve the repo's canonical assets straight from the parent repo (read in place, NOT copied)
// so the editors' schema/builtin fetches and the replay viewer's /schemas + /runs loads resolve during
// `vite dev`. Each prefix maps 1:1 to a repo-root folder; production bundling is out of scope here.
function serveRepoAssets(): Plugin {
  const ROOT = resolve(fileURLToPath(new URL("..", import.meta.url)));
  const DIRS = ["schemas", "config", "scenarios", "runs"];
  const BASES = DIRS.map((d) => resolve(ROOT, d) + sep);
  const TYPES: Record<string, string> = {
    ".json": "application/json",
    ".png": "image/png",
    ".svg": "image/svg+xml",
  };
  return {
    name: "serve-repo-assets",
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
  };
}

export default defineConfig({
  plugins: [react(), serveRepoAssets()],
  server: { port: 5173 },
});
