// The session stats overlay's small pure parts: saved preferences, corners, the codec tag. What the
// panel says (rows, labels, number formats) is in @cha/ui-spec's stats-panel.json.
import { formatNumber } from "@cha/ui-spec";

export type Corner = "top-left" | "top-right" | "bottom-left" | "bottom-right";
/** Clockwise from the top left, which is the order the arrow keys walk. */
export const CORNERS: Corner[] = ["top-left", "top-right", "bottom-right", "bottom-left"];

/** The full view's sections, in the order they are shown. */
export const SECTIONS = ["stream", "latency", "network", "node"] as const;
export type SectionId = (typeof SECTIONS)[number];

export interface OverlayPrefs {
  open: boolean;
  compact: boolean;
  collapsed: boolean;
  corner: Corner;
  /** Sections folded to their heading. */
  folded: SectionId[];
  /** Panel background opacity, in percent. */
  opacity: number;
  /** On: it snaps to the nearest corner. Off: it stays where it is dropped (`pos`). */
  snap: boolean;
  /** Where free placement left the panel's top left, in the video area's pixels; null until it is moved. */
  pos: { left: number; top: number } | null;
}

export const OPACITY_MIN = 30;

export const DEFAULT_PREFS: OverlayPrefs = { open: true, compact: false, collapsed: false, corner: "top-left", folded: [], opacity: 90, snap: true, pos: null };
export const PREFS_KEY = "cha.statsOverlay";

/** Saved preferences from JSON text; anything missing or malformed falls back to the default. */
export function parsePrefs(raw: string | null): OverlayPrefs {
  const out = { ...DEFAULT_PREFS };
  if (!raw) return out;
  try {
    const v = JSON.parse(raw) as Record<string, unknown>;
    if (typeof v.open === "boolean") out.open = v.open;
    if (typeof v.compact === "boolean") out.compact = v.compact;
    if (typeof v.collapsed === "boolean") out.collapsed = v.collapsed;
    if (CORNERS.includes(v.corner as Corner)) out.corner = v.corner as Corner;
    if (typeof v.opacity === "number" && Number.isFinite(v.opacity)) out.opacity = Math.min(100, Math.max(OPACITY_MIN, Math.round(v.opacity)));
    if (typeof v.snap === "boolean") out.snap = v.snap;
    const pos = v.pos as { left?: unknown; top?: unknown } | null | undefined;
    if (pos && Number.isFinite(pos.left) && Number.isFinite(pos.top)) out.pos = { left: pos.left as number, top: pos.top as number };
    if (Array.isArray(v.folded)) out.folded = SECTIONS.filter((id) => (v.folded as unknown[]).includes(id));
  } catch {
    // keep the defaults
  }
  return out;
}

/** `pos` moved to lie wholly inside a `width`×`height` area for a panel of `w`×`h` (flush to the top left if it can't fit). */
export function clampPos(pos: { left: number; top: number }, w: number, h: number, width: number, height: number) {
  return { left: Math.min(Math.max(0, pos.left), Math.max(0, width - w)), top: Math.min(Math.max(0, pos.top), Math.max(0, height - h)) };
}

/** The corner whose side of the area the point (the panel's centre) is on. */
export function nearestCorner(x: number, y: number, width: number, height: number): Corner {
  return `${y < height / 2 ? "top" : "bottom"}-${x < width / 2 ? "left" : "right"}` as Corner;
}

/** The corner after `from` in the arrow key's direction: left/right swap sides, up/down swap rows. */
export function cornerByArrow(from: Corner, key: string): Corner {
  let [row, side] = from.split("-") as ["top" | "bottom", "left" | "right"];
  if (key === "ArrowLeft") side = "left";
  else if (key === "ArrowRight") side = "right";
  else if (key === "ArrowUp") row = "top";
  else if (key === "ArrowDown") row = "bottom";
  return `${row}-${side}` as Corner;
}

/** A number with fixed digits, or an en dash when there is none (the panel's formatter, for text outside the spec). */
export const num = formatNumber;

/** "HEVC/WT": the codec and a short transport name, whichever are known. */
export function codecTag(codec: string | null, transport: "webtransport" | "webrtc" | null): string {
  const t = transport === "webtransport" ? "WT" : transport === "webrtc" ? "RTC" : "";
  // The player's stats name the transport after the codec ("HEVC · WebTransport"): keep the codec.
  const name = codec?.split(" · ")[0]?.trim().toUpperCase() ?? "";
  return [name, t].filter(Boolean).join("/");
}
