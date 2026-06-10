# One-command headless-browser smoke for the wasm Play view.
#
# WHY a helper: vite dev cannot run from the Z: UNC mount (the space in "\\host\Shared Folders" breaks
# chokidar's fs.watch with EISDIR and esbuild's dep path resolution). So this copies the repo to a LOCAL
# no-space path, starts vite there, and runs web/tests/play.smoke.mjs against system Chrome via
# playwright-core (no browser download). Re-runs are fast (robocopy is incremental).
#
# Requires: Node + npm, Google Chrome installed, and the wasm pkg built
#   (wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg).
#
#   pwsh tools/web-smoke.ps1                 # uses C:\temp\osc-smoke, port 5174
#   pwsh tools/web-smoke.ps1 -Local D:\x -Port 5180
param(
  [string]$Local = "C:\temp\osc-smoke",
  [int]$Port = 5174,
  [string]$Chrome = "C:\Program Files\Google\Chrome\Application\chrome.exe"
)
$ErrorActionPreference = "Stop"
$Repo = Split-Path -Parent $PSScriptRoot   # tools/ -> repo root

if (-not (Test-Path $Chrome)) { Write-Error "Chrome not found at $Chrome — pass -Chrome <path>"; exit 2 }
if (-not (Test-Path (Join-Path $Repo "web\src\engine\pkg\openstratcore_wasm_bg.wasm"))) {
  Write-Error "wasm pkg missing — run: wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg"
  exit 2
}

# 1. Mirror the web app + the assets its dev middleware serves, to a local no-space path (incremental).
Write-Host "==> copying to $Local (incremental)"
foreach ($d in @("web", "schemas", "config", "scenarios", "runs")) {
  robocopy (Join-Path $Repo $d) (Join-Path $Local $d) /E /XD node_modules\.cache /MT:16 /R:0 /W:0 /NFL /NDL /NJH /NJS /NP | Out-Null
}
# node_modules is needed but huge — mirror it once (skipped fast on re-runs).
robocopy (Join-Path $Repo "web\node_modules") (Join-Path $Local "web\node_modules") /E /MT:16 /R:0 /W:0 /NFL /NDL /NJH /NJS /NP | Out-Null

$webLocal = Join-Path $Local "web"
Push-Location $webLocal
try {
  # 2. Ensure playwright-core (drives system Chrome; no browser download).
  if (-not (Test-Path (Join-Path $webLocal "node_modules\playwright-core"))) {
    Write-Host "==> npm install playwright-core"
    npm install --no-audit --no-fund playwright-core | Out-Null
  }

  # 3. Start vite dev in the background.
  Write-Host "==> starting vite on :$Port"
  $vite = Start-Process -FilePath "npm" -ArgumentList @("run", "dev", "--", "--port", "$Port") `
    -WorkingDirectory $webLocal -PassThru -WindowStyle Hidden
  try {
    # 4. Wait for it to serve.
    $up = $false
    for ($i = 0; $i -lt 40; $i++) {
      try { Invoke-WebRequest "http://localhost:$Port/" -UseBasicParsing -TimeoutSec 2 | Out-Null; $up = $true; break }
      catch { Start-Sleep -Milliseconds 500 }
    }
    if (-not $up) { Write-Error "vite did not come up on :$Port"; exit 1 }

    # 5. Run the smoke.
    $env:PLAY_SMOKE_URL = "http://localhost:$Port/"
    $env:PLAY_SMOKE_CHROME = $Chrome
    $env:PLAY_SMOKE_OUT = $Local
    Write-Host "==> running play.smoke.mjs (screenshots -> $Local)"
    node (Join-Path $webLocal "tests\play.smoke.mjs")
    $code = $LASTEXITCODE
  }
  finally {
    if ($vite -and -not $vite.HasExited) { Stop-Process -Id $vite.Id -Force -ErrorAction SilentlyContinue }
    Get-Process node -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "*$Local*" } | Stop-Process -Force -ErrorAction SilentlyContinue
  }
}
finally { Pop-Location }

Write-Host "==> screenshots: $Local\play-full.png , $Local\play-canvas.png"
exit $code
