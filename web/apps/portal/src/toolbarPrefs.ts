// The session toolbar's settings, remembered per user and per app type (the template): what you
// last picked for Chrome is not what you last picked for Steam. Kept in this browser's localStorage.
// The fields, defaults and limits are prefs.json's (@cha/ui-spec), shared with Cha Player.
import { parsePrefsText } from "@cha/ui-spec";

export interface ToolbarPrefs {
  codec?: string;
  transport?: "auto" | "webrtc" | "webtransport" | "websocket";
  muted?: boolean;
  /** 0..100. */
  volume?: number;
  /** False: the mouse was switched off. */
  mouse?: boolean;
  fps?: number;
  /** The Steam Gamescope overlay level. */
  overlay?: number;
}

export const toolbarKey = (userId: string, templateId: string) => `cha.toolbar.${userId}.${templateId}`;

/** Saved settings from JSON text; each field is kept only if it is well formed (prefs.json). */
export function parseToolbarPrefs(raw: string | null): ToolbarPrefs {
  return parsePrefsText("toolbar", raw, "web") as ToolbarPrefs;
}

export function loadToolbarPrefs(key: string): ToolbarPrefs {
  try {
    return parseToolbarPrefs(localStorage.getItem(key));
  } catch {
    return {};
  }
}

/** Merge `patch` into what is saved under `key`. */
export function saveToolbarPrefs(key: string, patch: ToolbarPrefs): void {
  try {
    localStorage.setItem(key, JSON.stringify({ ...loadToolbarPrefs(key), ...patch }));
  } catch {
    // private window or blocked storage: the toolbar just starts at its defaults next time
  }
}
