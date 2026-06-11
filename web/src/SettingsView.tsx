import { useState } from "react";
import { loadSettings, saveSettings, type AppSettings } from "./settings";

// Configure an LLM commander + game settings. Everything is stored in your browser's localStorage only;
// the API key is sent ONLY to the endpoint you enter (directly from your browser), never to us.
export function SettingsView() {
  const [s, setS] = useState<AppSettings>(loadSettings());
  const [saved, setSaved] = useState(false);

  function patch(next: Partial<AppSettings>) { const v = { ...s, ...next }; setS(v); saveSettings(v); setSaved(true); }
  function patchLlm(next: Partial<AppSettings["llm"]>) { patch({ llm: { ...s.llm, ...next } }); }

  const card: React.CSSProperties = { padding: "14px 16px", borderRadius: 8, background: "#141c26", border: "1px solid #243042", marginBottom: 16, maxWidth: 620 };
  const label: React.CSSProperties = { display: "block", margin: "10px 0 4px", fontSize: 13, opacity: 0.85 };
  const input: React.CSSProperties = { width: "100%", boxSizing: "border-box", padding: "7px 9px", borderRadius: 6, border: "1px solid #2c3a4d", background: "#0e141c", color: "#e8edf3" };

  return (
    <div style={{ marginTop: 16 }}>
      <h2 style={{ marginTop: 0 }}>设置</h2>

      <div style={card}>
        <div style={{ fontWeight: 700, marginBottom: 4 }}>🤖 大模型指挥官 (LLM)</div>
        <div style={{ fontSize: 13, opacity: 0.8 }}>让一个大模型指挥一方。配置仅存在你的浏览器；API Key 只会从你的浏览器直接发往你填的 endpoint。</div>

        <label style={{ display: "flex", alignItems: "center", gap: 8, margin: "10px 0 0" }}>
          <input type="checkbox" checked={s.llm.enabled} onChange={(e) => patchLlm({ enabled: e.target.checked })} />
          启用 LLM 指挥官
        </label>

        <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
          <div style={{ flex: "1 1 180px" }}>
            <label style={label}>指挥哪一方</label>
            <select style={input} value={s.llm.side} onChange={(e) => patchLlm({ side: e.target.value as "red" | "blue" })}>
              <option value="blue">蓝方 (推荐：你打红方)</option>
              <option value="red">红方</option>
            </select>
          </div>
          <div style={{ flex: "1 1 180px" }}>
            <label style={label}>提供方 / 协议</label>
            <select style={input} value={s.llm.provider} onChange={(e) => patchLlm({ provider: e.target.value as "anthropic" | "openai" })}>
              <option value="anthropic">Anthropic (Claude)</option>
              <option value="openai">OpenAI 兼容 (OpenAI / 本地代理 / vLLM…)</option>
            </select>
          </div>
        </div>

        <label style={label}>API 地址 (Base URL)</label>
        <input style={input} value={s.llm.baseUrl} placeholder={s.llm.provider === "anthropic" ? "留空=https://api.anthropic.com" : "如 https://api.openai.com/v1 或 http://localhost:11434/v1"} onChange={(e) => patchLlm({ baseUrl: e.target.value.trim() })} />

        <label style={label}>API Key</label>
        <input style={input} type="password" value={s.llm.apiKey} placeholder="sk-... (仅存本地浏览器)" onChange={(e) => patchLlm({ apiKey: e.target.value.trim() })} />

        <label style={label}>模型名</label>
        <input style={input} value={s.llm.model} placeholder={s.llm.provider === "anthropic" ? "claude-opus-4-8" : "gpt-4o / 本地模型名"} onChange={(e) => patchLlm({ model: e.target.value.trim() })} />

        <div style={{ fontSize: 12, opacity: 0.65, marginTop: 10 }}>
          ⚠️ 浏览器直连需对方 endpoint 允许 CORS：Anthropic 已自动加 <code>anthropic-dangerous-direct-browser-access</code> 头；
          OpenAI 官方端点通常禁止浏览器直连，建议用本地代理 / vLLM / 兼容服务。LLM 出错时会自动回退到内置脚本 AI。
        </div>
      </div>

      <div style={card}>
        <div style={{ fontWeight: 700, marginBottom: 6 }}>🎮 游戏设置</div>
        <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <input type="checkbox" checked={s.autoSaveReplay} onChange={(e) => patch({ autoSaveReplay: e.target.checked })} />
          对局结束时自动保存复盘 (.replay.json 下载，可在「复盘」标签载入)
        </label>
      </div>

      {saved && <div style={{ color: "#9ee29e" }}>✓ 已保存（自动写入浏览器）。回到「对局」即生效。</div>}
    </div>
  );
}
