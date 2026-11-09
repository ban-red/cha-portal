// The screen's refresh rate, and whether a stream's frame rate fits it (docs/plans/hdr-and-vrr.md,
// step 4). A stream at a rate the screen's refresh isn't a multiple of can't hold every frame for
// the same time: 90 fps on a 120 Hz panel shows frames for 1, 1 and 2 refreshes in turn, a judder
// at 30 Hz. Chrome on a ProMotion Mac only ever runs at 120, 60 or 30 Hz (research 12), so there
// 60 and 120 fit and 90 doesn't.

/** How a stream's rate fits a screen: "" evenly, "uneven" (it judders), or "over" (faster than the screen shows). */
export type Fit = "" | "uneven" | "over";

/**
 * How a stream at `fps` fits a screen refreshing at `hz` (null: not measured, so nothing is said).
 * A screen with variable refresh (`vrr`) follows any rate up to its own.
 */
export function fitOf(fps: number, hz: number | null, vrr = false): Fit {
  if (!hz || hz <= 0 || fps <= 0) return "";
  // A measured rate is within a few percent of the true one: compare with that slack.
  if (fps > hz * 1.03) return "over";
  if (vrr) return "";
  const per = hz / fps;
  return Math.abs(per - Math.round(per)) <= 0.03 * per ? "" : "uneven";
}

/** Refresh rates screens run at; a measured rate within 3% of one is that one. */
const RATES = [24, 30, 48, 50, 60, 72, 75, 90, 100, 120, 144, 165, 170, 175, 180, 200, 240, 360];

/**
 * The screen's refresh rate from `requestAnimationFrame` intervals, ms: the median interval as a
 * rate, snapped to a common one. Null with too few samples to tell.
 */
export function refreshFromIntervals(intervalsMs: readonly number[]): number | null {
  const usable = intervalsMs.filter((ms) => ms > 1 && ms < 100).sort((a, b) => a - b);
  if (usable.length < 10) return null;
  const median = usable[Math.floor(usable.length / 2)]!;
  const hz = 1000 / median;
  const near = RATES.find((r) => Math.abs(hz - r) / r <= 0.03);
  return near ?? Math.round(hz);
}

let measuring: Promise<number | null> | null = null;

/**
 * The screen's refresh rate, sampled once per page from about a second and a half of
 * `requestAnimationFrame` (null where there is none, or in a hidden tab, whose rAF is throttled).
 * The player and the portal share the one measurement.
 */
export function measureRefresh(frames = 90): Promise<number | null> {
  if (measuring) return measuring;
  measuring = new Promise((resolve) => {
    if (typeof requestAnimationFrame !== "function") return resolve(null);
    const intervals: number[] = [];
    let last: number | null = null;
    const tick = (now: number) => {
      if (last !== null) intervals.push(now - last);
      last = now;
      if (intervals.length < frames) requestAnimationFrame(tick);
      else resolve(refreshFromIntervals(intervals));
    };
    requestAnimationFrame(tick);
  });
  return measuring;
}
