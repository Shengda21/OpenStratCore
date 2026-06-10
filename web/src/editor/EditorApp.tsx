// EditorApp.tsx — 三类图形化编辑器的容器（地图 / 想定 / 规则）
import { useState } from "react";
import { MapEditor } from "./MapEditor";
import { ScenarioEditor } from "./ScenarioEditor";
import { RulesEditor } from "./RulesEditor";

type Tab = "map" | "scenario" | "rules";

export function EditorApp() {
  const [tab, setTab] = useState<Tab>("map");
  return (
    <div className="editor-app">
      <nav className="tabs">
        <button onClick={() => setTab("map")} aria-pressed={tab === "map"}>地图</button>
        <button onClick={() => setTab("scenario")} aria-pressed={tab === "scenario"}>想定</button>
        <button onClick={() => setTab("rules")} aria-pressed={tab === "rules"}>规则</button>
      </nav>
      <p className="hint">
        内置资源（内置规则 + 跑通所需最小集）只读；在此基础上可手动编辑或从文本/JSON 导入，导出为新的用户资源。详见 docs/RESOURCES.md。
      </p>
      {tab === "map" && <MapEditor />}
      {tab === "scenario" && <ScenarioEditor />}
      {tab === "rules" && <RulesEditor />}
    </div>
  );
}
