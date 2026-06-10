// Thin wrapper over the wasm-built kernel (crates/openstratcore-wasm) — the browser runs the SAME Rust
// engine. Build the pkg first (it is generated, gitignored):
//   wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg
// This wrapper + the Play view are the only modules that import ./pkg.
import init, { Engine } from "./pkg/openstratcore_wasm";

let ready: Promise<void> | null = null;

// Initialize the wasm module exactly once (idempotent). Await before constructing an Engine.
export function initEngine(): Promise<void> {
  if (!ready) ready = init().then(() => undefined);
  return ready;
}

export { Engine };
