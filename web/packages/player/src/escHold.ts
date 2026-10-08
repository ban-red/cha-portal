// Hold Esc to let go of exclusive input. While the pointer is captured and Esc reaches the page,
// Esc goes to the host at once, as every key does. Held for `holdMs` (toolbar.json's
// timing.release_hold_ms), it also releases the pointer: Esc's key-up goes to the host then, and
// the user's real key-up is swallowed. After `hintMs` of the hold a progress bar tells them to keep
// holding. Pure state with the time passed in, so a fake clock tests it; the same rules run in
// crates/cha-ui-spec/src/esc_hold.rs, and both pass web/packages/ui-spec/capture-cases.json.
//
//   idle ──down──▶ holding ──tick ≥ holdMs──▶ released (key-up swallowed) ──down──▶ holding
//                     │                          
//                     └──up / blur / uncapture──▶ idle

/** What an event did: the Esc key event to send to the host, and whether to release the pointer. */
export interface EscStep {
  host: "down" | "up" | null;
  /** Let go of the pointer and the capture, as the toolbar's exclusive-input button does. */
  release: boolean;
}

const NONE: EscStep = { host: null, release: false };

export class EscHold {
  private downAt: number | null = null;
  /** The next key-up is ours: it ends a hold that released, or a press a blur cancelled. */
  private swallowUp = false;

  constructor(
    private readonly holdMs: number,
    private readonly hintMs: number,
  ) {}

  /** Esc is down and the user has not let go (and nothing has released yet). */
  get holding(): boolean {
    return this.downAt !== null;
  }

  /**
   * An Esc keydown. `captured`: the pointer is captured and Esc reaches the page. Repeats of a held
   * key are dropped (the host repeats keys itself).
   */
  keyDown(now: number, repeat: boolean, captured: boolean): EscStep {
    if (repeat) return NONE;
    this.swallowUp = false;
    if (!captured) {
      this.downAt = null;
      return { host: "down", release: false };
    }
    if (this.downAt !== null) return NONE;
    this.downAt = now;
    return { host: "down", release: false };
  }

  keyUp(now: number): EscStep {
    // The timer may have been late: a hold that lasted long enough releases.
    const fired = this.tick(now);
    if (fired.release) return fired;
    if (this.downAt !== null) {
      this.downAt = null;
      return { host: "up", release: false };
    }
    if (this.swallowUp) {
      this.swallowUp = false;
      return NONE;
    }
    return { host: "up", release: false };
  }

  /** Time passed (the timer, or a redraw). */
  tick(now: number): EscStep {
    if (this.downAt === null || now - this.downAt < this.holdMs) return NONE;
    this.downAt = null;
    this.swallowUp = true;
    return { host: "up", release: true };
  }

  /** The window lost focus. The player's release-all sends Esc's key-up; the real one is dropped. */
  blur(): EscStep {
    if (this.downAt === null) return NONE;
    this.downAt = null;
    this.swallowUp = true;
    return { host: "up", release: false };
  }

  /** The pointer was released some other way: the hold is moot, and the key-up goes on normally. */
  uncapture(): void {
    this.downAt = null;
    this.swallowUp = false;
  }

  /** The hold's progress, 0..1, once the hint is due; null while it is hidden. */
  hint(now: number): number | null {
    if (this.downAt === null || now - this.downAt < this.hintMs) return null;
    return Math.min(1, (now - this.downAt) / this.holdMs);
  }

  /** When the state changes by itself next (the hint appears, the hold fires); null if it won't. */
  due(): number | null {
    if (this.downAt === null) return null;
    return this.downAt + this.holdMs;
  }

  /** When the hint is due, for a timer; null when not holding. */
  hintDue(): number | null {
    return this.downAt === null ? null : this.downAt + this.hintMs;
  }
}
