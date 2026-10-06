/** The performance overlay levels the page can ask for: 0 off, 1 FPS only, 2 bar, 3 extended, 4 full. */
export const OVERLAY_LEVELS = [0, 1, 2, 3, 4] as const;
export type OverlayLevel = (typeof OVERLAY_LEVELS)[number];

/** What the streamer reports: a level, or `"custom"` for a config somebody wrote by hand. */
export type OverlayState = OverlayLevel | "custom";

/**
 * Reads the streamer's `overlay` field (hello and stats) or an answer's `level`. Anything that
 * isn't a level or `"custom"` is `null`: the app has no overlay (the streamer leaves the field out).
 */
export function parseOverlay(value: unknown): OverlayState | null {
  if (value === "custom") return "custom";
  return typeof value === "number" && (OVERLAY_LEVELS as readonly number[]).includes(value) ? (value as OverlayLevel) : null;
}

/**
 * What the answer to a `{"t":"overlay"}` request says: the level now, or the streamer's reason it
 * didn't change.
 */
export function overlayAnswer(msg: { level?: unknown; error?: string }): { level: OverlayState | null; error?: string } {
  return { level: parseOverlay(msg.level), error: msg.error || undefined };
}
