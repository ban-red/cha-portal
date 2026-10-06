// The session toolbar's settings, remembered per user and per app type (the template): what you
// last picked for Chrome is not what you last picked for Steam. Kept in this browser's localStorage.

export interface ToolbarPrefs {
  codec?: string;
  transport?: "auto" | "webrtc" | "webtransport";
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

const TRANSPORTS = ["auto", "webrtc", "webtransport"];

/** Saved settings from JSON text; each field is kept only if it is well formed. */
export function parseToolbarPrefs(raw: string | null): ToolbarPrefs {
  const out: ToolbarPrefs = {};
  if (!raw) return out;
  try {
    const v = JSON.parse(raw) as Record<string, unknown>;
    if (typeof v.codec === "string" && v.codec) out.codec = v.codec;
    if (typeof v.transport === "string" && TRANSPORTS.includes(v.transport)) out.transport = v.transport as ToolbarPrefs["transport"];
    if (typeof v.muted === "boolean") out.muted = v.muted;
    if (typeof v.volume === "number" && Number.isFinite(v.volume)) out.volume = Math.min(100, Math.max(0, Math.round(v.volume)));
    if (typeof v.mouse === "boolean") out.mouse = v.mouse;
    if (typeof v.fps === "number" && Number.isFinite(v.fps) && v.fps > 0) out.fps = v.fps;
    if (typeof v.overlay === "number" && Number.isInteger(v.overlay) && v.overlay >= 0) out.overlay = v.overlay;
  } catch {
    // keep what was read
  }
  return out;
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
