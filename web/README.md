# OpenStratCore Web

TS/React + PixiJS 前端：**对局视图**（浏览器内直跑 wasm 内核，逐 tick 推演 + 战争迷雾渲染）、
**地图/想定/规则三类编辑器**、**复盘查看器**。同一份确定性 Rust 内核经 wasm-bindgen 跑在浏览器里。

## 普通玩家：下载即玩（零工具链）

从 **GitHub Releases** 下载 `openstratcore-web-<版本>.zip`，解压后用任意静态服务器伺服：
```bash
python -m http.server -d openstratcore-web-<版本> 8000
# 打开 http://localhost:8000
```
构建产物 `dist/` 是**自包含**的——`/schemas`、`/config`、`/scenarios`、`/runs`、`/assets` 都已打进包，
不需要任何后端或开发服务器。

## 开发者：本地构建

前置：**Rust(stable)** + [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) + **Node 20+**。

```bash
# 1) 把确定性内核编成 wasm（产物 web/src/engine/pkg/，已 gitignore）
wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg

cd web
npm install

# 2a) 开发服务器（热更新；dev 中间件直接从仓库供应 /schemas /config /scenarios /assets）
npm run dev            # http://localhost:5173

# 2b) 生产构建（自包含静态包；vite 插件把上述目录拷进 dist/）
npm run build          # -> web/dist/
npm run preview        # 本地预览构建产物
```

> **Windows 共享盘注意**：若仓库在带空格的 UNC 路径（如 `\\host\Shared Folders\…`，常见于 VMware 共享盘
> 映射的 Z: 盘），`vite build`/`vite dev` 会因 rollup/esbuild/chokidar 解析含空格的真实路径而失败
> （`tsc` 仍通过）。解决：把仓库**复制到无空格的本地路径**（如 `C:\temp\osc`）再构建。代码本身是 build-clean 的。

## 资产与契约
- 运行时 fetch 的数据均来自仓库真源：`/schemas`（编辑器 ajv 校验）、`/config`（规则 + 18 张裁决表）、
  `/scenarios`（+ 地图）、`/assets/generated`（单位/地形贴图 + manifest）。
- 复盘查看器内置样例：`/runs/demo_skirmish.replay.json`（随 `web/public/` 发布）。
- 构建期由 `vite.config.ts` 的 `repoAssets` 插件把这些目录拷进 `dist/`；`dev` 下由同一插件就地供应。

## 烟测
`pwsh tools/web-smoke.ps1`（仓库根）：把 web 应用镜像到无空格本地路径、起 vite、用 playwright-core
驱动系统 Chrome 验证对局视图 wasm 内核初始化 + 推进 + canvas 非空。需先构建 wasm pkg。
