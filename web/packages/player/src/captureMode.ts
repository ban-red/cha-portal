// Mouse capture, as the user sees it: once they capture the mouse, a release
// by the browser (Esc, in Safari and Firefox) is not the end of it. A click on
// the picture captures again, and that click is not a click in the app.
// Pure state; input.ts does the pointer lock and the timer.
//
//   idle ──lock──▶ capturing ──browser unlocks──▶ released ──lock──▶ capturing
//     ▲                                              │
//     └──────────────────── turnOff ◀────────────────┘

export type CaptureState = "idle" | "capturing" | "released";

export interface CaptureView {
  state: CaptureState;
  /** Recapture mode: clicks on the picture capture the mouse again. */
  recapture: boolean;
  /** Show "Click to capture the mouse". */
  hint: boolean;
}

export class CaptureMode {
  private s: CaptureState = "idle";
  private hintOn = false;
  /** Buttons whose press recaptured the mouse: their release is ours too. */
  private readonly swallowed = new Set<number>();

  get state(): CaptureState {
    return this.s;
  }

  get view(): CaptureView {
    return { state: this.s, recapture: this.s === "released", hint: this.hintOn };
  }

  /** The pointer lock was granted (by the button, by a recapture click). */
  locked(): void {
    this.s = "capturing";
    this.hintOn = false;
  }

  /** The pointer lock ended without us asking: the browser (Esc) did it. */
  unlocked(): void {
    if (this.s !== "capturing") return;
    this.s = "released";
    this.hintOn = true;
  }

  /** The lock ended because this page let go. Nothing to recapture. */
  releasedByUs(): void {
    this.s = "idle";
    this.hintOn = false;
  }

  /** The user leaves recapture mode: clicks go to the stream again. */
  turnOff(): void {
    this.s = "idle";
    this.hintOn = false;
  }

  hideHint(): void {
    this.hintOn = false;
  }

  /** A press on the picture while not locked: `"recapture"` swallows it and captures. */
  pointerDown(button: number): "recapture" | "pass" {
    if (this.s !== "released" || button !== 0) return "pass";
    this.swallowed.add(button);
    return "recapture";
  }

  /** The matching release: `"swallow"` if its press was one. */
  pointerUp(button: number): "swallow" | "pass" {
    return this.swallowed.delete(button) ? "swallow" : "pass";
  }
}
