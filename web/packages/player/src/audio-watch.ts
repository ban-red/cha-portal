// Small pure pieces of the Web Audio output's self-healing and stats (audio-out.ts), kept apart
// so they test without a browser.

/** Pushed frames with none played for this long, while the context runs, is a dead output. */
export const STALL_MS = 2000;

/**
 * Notices an output that takes sound but plays none of it: the worklet died, or the context says
 * "running" and renders nothing. Fed the frames pushed and played (totals) and whether the
 * context is running; says true once the totals have shown pushes but no plays for `STALL_MS`.
 */
export class StallDetector {
  private since: number | null = null;
  private lastPushed = 0;
  private lastPlayed = 0;

  observe(now: number, pushed: number, played: number, running: boolean): boolean {
    const advanced = played > this.lastPlayed;
    const pushing = pushed > this.lastPushed;
    this.lastPlayed = played;
    this.lastPushed = pushed;
    if (!running || advanced) {
      this.since = null;
      return false;
    }
    // Pushing but not playing starts the clock; a quiet input (nothing pushed) is not a stall,
    // but an already started clock keeps running through a gap in pushes within the window.
    if (this.since === null) {
      if (pushing) this.since = now;
      return false;
    }
    return now - this.since >= STALL_MS;
  }

  reset(): void {
    this.since = null;
    this.lastPushed = 0;
    this.lastPlayed = 0;
  }
}

/** The largest of the values noted in the last `windowMs`. */
export class PeakWindow {
  private readonly items: { at: number; v: number }[] = [];

  constructor(private readonly windowMs = 1000) {}

  note(now: number, v: number): void {
    this.items.push({ at: now, v });
    this.trim(now);
  }

  /** The window's peak, or null with nothing noted in it. */
  peak(now: number): number | null {
    this.trim(now);
    if (!this.items.length) return null;
    return this.items.reduce((m, i) => Math.max(m, i.v), 0);
  }

  private trim(now: number): void {
    while (this.items.length && now - this.items[0]!.at > this.windowMs) this.items.shift();
  }
}

/**
 * What to do about a context that isn't running: nothing while muted, hidden or still waiting for
 * its first gesture; otherwise try to resume, and after `MAX_RESUME_TRIES` tries in a row that
 * didn't bring it back (or at once when closed), rebuild it.
 */
export const MAX_RESUME_TRIES = 4;

export function contextAction(opts: {
  state: string;
  wantSound: boolean;
  visible: boolean;
  /** It has run at least once: a gesture was given, so a suspend now is the browser's doing. */
  hasRun: boolean;
  resumeTries: number;
}): "none" | "resume" | "rebuild" {
  if (opts.state === "closed") return "rebuild";
  if (opts.state === "running") return "none";
  if (!opts.wantSound || !opts.visible) return "none";
  if (!opts.hasRun) return "resume"; // autoplay: harmless to try, can't be fixed by a rebuild
  return opts.resumeTries >= MAX_RESUME_TRIES ? "rebuild" : "resume";
}
