import { useState } from "react";
import { createRoot } from "react-dom/client";
import { PlayView } from "./play/PlayView";
import { ReplayViewer } from "./replay/ReplayViewer";
import { EditorApp } from "./editor/EditorApp";
import "./styles.css";

// Web 前端骨架。同一 Rust 内核经 wasm 在此直跑（crates/openstratcore-wasm）。先构建 pkg：
//   wasm-pack build crates/openstratcore-wasm --target web --out-dir web/src/engine/pkg
// 三种视图：对局(Play：wasm 直跑) / 复盘(Replay) / 编辑器(Editor：地图·想定·规则)。

type View = "play" | "replay" | "editor";

function App() {
  const [view, setView] = useState<View>("editor");
  return (
    <div className="app">
      <header className="topbar">
        <h1>OpenStratCore</h1>
        <nav>
          <button onClick={() => setView("play")} aria-pressed={view === "play"}>对局</button>
          <button onClick={() => setView("replay")} aria-pressed={view === "replay"}>复盘</button>
          <button onClick={() => setView("editor")} aria-pressed={view === "editor"}>编辑器</button>
        </nav>
      </header>
      {view === "play" && <PlayView />}
      {view === "replay" && <ReplayViewer />}
      {view === "editor" && <EditorApp />}
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
