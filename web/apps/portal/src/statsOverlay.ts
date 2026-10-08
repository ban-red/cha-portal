// The session stats overlay's small pure parts: saved preferences, corners, the codec tag. What the
// panel says (rows, labels, number formats) is in @cha/ui-spec's stats-panel.json.
import { formatNumber, PREFS, parsePrefsText } from "@cha/ui-spec";

const FIELDS = PREFS.stats_panel.fields;

/** Clockwise from the top left, which is the order the arrow keys walk. */
export const CORNERS = FIELDS.corner!.values as Corner[];
export type Corner = "top-left" | "top-right" | "bottom-left" | "bottom-right";

/** The full view's sections, in the order they are shown. */
export const SECTIONS = FIELDS.folded!.values as SectionId[];
export type SectionId = "stream" | "latency" | "network" | "node";

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

export const OPACITY_MIN = FIELDS.opacity!.min!;

export const PREFS_KEY = "cha.statsOverlay";

/** Saved preferences from JSON text; each field that is missing or malformed falls back to its default (prefs.json). */
export function parsePrefs(raw: string | null): OverlayPrefs {
  return parsePrefsText("stats_panel", raw, "web") as unknown as OverlayPrefs;
}

/** Nothing saved. */
export const DEFAULT_PREFS: OverlayPrefs = parsePrefs(null);

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
