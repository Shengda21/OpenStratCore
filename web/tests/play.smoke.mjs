// Headless-browser smoke for the wasm Play view. Drives system Chrome via playwright-core (no browser
// download), loads the running app, switches to 对局, waits for the wasm engine to init, steps the
// clock, and asserts the PIXI canvas rendered non-blank content + the clock advanced. Writes
// screenshots as visual artifacts. Exits non-zero on failure.
//
// This canNOT run from the Z: UNC mount (vite dev breaks on the space in "Shared Folders"); run it from
// a local no-space copy via tools/web-smoke.ps1, which copies the repo, starts vite, and invokes this.
//
// Env: PLAY_SMOKE_URL (default http://localhost:5174/), PLAY_SMOKE_CHROME (default standard install
// path), PLAY_SMOKE_OUT (screenshot dir, default cwd).
import { chromium } from "playwright-core";
import { join } from "node:path";

const URL = process.env.PLAY_SMOKE_URL || "http://localhost:5174/";
const CHROME = process.env.PLAY_SMOKE_CHROME || "C:/Program Files/Google/Chrome/Application/chrome.exe";
const OUT = process.env.PLAY_SMOKE_OUT || ".";

const errors = [];
let ok = false;
const browser = await chromium.launch({
  executablePath: CHROME,
  headless: true,
  args: ["--use-gl=angle", "--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
});
try {
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } });
  page.on("console", (m) => { if (m.type() === "error") errors.push("console: " + m.text()); });
  page.on("pageerror", (e) => errors.push("pageerror: " + e.message));

  await page.goto(URL, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "对局" }).click();

  // Wait for the wasm engine to be ready: the 前进 button becomes enabled (PlayView sets hasEngine).
  await page.waitForFunction(() => {
    const b = [...document.querySelectorAll("button")].find((x) => /前进/.test(x.textContent || ""));
    return b && !b.disabled;
  }, { timeout: 25000 });

  const clockText = () => page.evaluate(() => {
    const el = [...document.querySelectorAll("span")].find((s) => /clock =/.test(s.textContent || ""));
    return el ? el.textContent.trim() : null;
  });
  const before = await clockText();
  await page.getByRole("button", { name: /前进/ }).click();
  await page.waitForTimeout(600);
  const after = await clockText();

  const canvas = page.locator("canvas").first();
  await canvas.waitFor({ state: "visible", timeout: 5000 });
  const box = await canvas.boundingBox();
  const canvasShot = await canvas.screenshot({ path: join(OUT, "play-canvas.png") });
  await page.screenshot({ path: join(OUT, "play-full.png") });

  // A blank solid 760x440 PNG is ~1-2KB; a rendered hex grid + unit symbols is larger.
  const advanced = !!before && !!after && before !== after && /clock = 5s/.test(after);
  const nonBlank = canvasShot.length > 3000;
  const hasCanvas = !!box && box.width > 100 && box.height > 100;

  console.log("clock before  :", before);
  console.log("clock after   :", after);
  console.log("canvas size   :", box ? `${Math.round(box.width)}x${Math.round(box.height)}` : "none");
  console.log("canvas png byte:", canvasShot.length);
  console.log("page errors   :", errors.length ? errors : "none (favicon 404 is harmless)");
  ok = advanced && nonBlank && hasCanvas;
  console.log("RESULT:", ok
    ? "PASS — wasm kernel rendered the 态势 and stepped the clock in a real browser"
    : `FAIL (advanced=${advanced} nonBlank=${nonBlank} hasCanvas=${hasCanvas})`);
} catch (e) {
  console.log("page errors   :", errors.length ? errors : "none");
  console.log("RESULT: FAIL (exception):", e.message);
} finally {
  await browser.close();
}
process.exit(ok ? 0 : 1);
