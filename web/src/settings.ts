// App settings persisted in localStorage (browser-only; nothing is sent anywhere except, if you enable
// it, directly to the LLM endpoint you configure). Used by the Settings view and the Play view.
import type { LlmConfig } from "./llm";

export interface LlmSettings extends LlmConfig {
  enabled: boolean;
  side: "red" | "blue"; // which side the LLM commands
}
export interface AppSettings {
  llm: LlmSettings;
  autoSaveReplay: boolean; // auto-download a .replay.json when a match ends
}

const KEY = "openstratcore.settings.v1";

export const DEFAULT_SETTINGS: AppSettings = {
  llm: { enabled: false, side: "blue", provider: "anthropic", baseUrl: "", apiKey: "", model: "claude-opus-4-8" },
  autoSaveReplay: true,
};

export function loadSettings(): AppSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_SETTINGS;
    const p = JSON.parse(raw) as Partial<AppSettings>;
    return { ...DEFAULT_SETTINGS, ...p, llm: { ...DEFAULT_SETTINGS.llm, ...(p.llm ?? {}) } };
  } catch { return DEFAULT_SETTINGS; }
}

export function saveSettings(s: AppSettings): void {
  try { localStorage.setItem(KEY, JSON.stringify(s)); } catch { /* private mode / quota — ignore */ }
}
