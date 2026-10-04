// Keyboard and mouse from the <video> element to the environment, on the
// control channel. Two pointer modes:
// - absolute (desktops): positions normalized to the picture;
// - locked (games): raw relative motion, scaled to stream pixels.
// Keys go by `KeyboardEvent.code` (the physical key); the streamer maps them.

type Send = (msg: Record<string, unknown>) => void;

export class InputCapture {
  private readonly keys = new Set<string>();
  private readonly buttons = new Set<number>();
  private readonly cleanup: (() => void)[] = [];

  constructor(
    private readonly video: HTMLVideoElement,
    private readonly send: Send,
  ) {
    const on = <K extends keyof HTMLElementEventMap>(
      target: HTMLElement | Window | Document,
      type: K | string,
      handler: (e: never) => void,
      options?: AddEventListenerOptions,
    ) => {
      target.addEventListener(type, handler as EventListener, options);
      this.cleanup.push(() => target.removeEventListener(type, handler as EventListener, options));
    };
    video.tabIndex = 0;
    video.style.cursor = "none";
    // Every mouse delta, not one per animation frame.
    const moveEvent = "onpointerrawupdate" in video ? "pointerrawupdate" : "pointermove";
    on(video, moveEvent, (e: PointerEvent) => this.move(e));
    on(video, "pointerdown", (e: PointerEvent) => this.button(e, true));
    on(video, "pointerup", (e: PointerEvent) => this.button(e, false));
    on(video, "wheel", (e: WheelEvent) => this.wheel(e), { passive: false });
    on(video, "contextmenu", (e: Event) => e.preventDefault());
    on(video, "keydown", (e: KeyboardEvent) => this.key(e, true));
    on(video, "keyup", (e: KeyboardEvent) => this.key(e, false));
    // Losing focus mid-press would leave keys and buttons stuck down remotely.
    on(video, "blur", () => this.releaseAll());
    on(window, "blur", () => this.releaseAll());
    on(document, "pointerlockchange", () => this.releaseAll());
  }

  get locked(): boolean {
    return document.pointerLockElement === this.video;
  }

  /** Raw mouse for games: the pointer disappears and moves relatively. */
  async lockPointer(): Promise<void> {
    try {
      await this.video.requestPointerLock({ unadjustedMovement: true });
    } catch {
      // No raw input here (e.g. Chrome on Linux): accelerated is still usable.
      await this.video.requestPointerLock();
    }
  }

  dispose(): void {
    this.releaseAll();
    this.cleanup.forEach((f) => f());
    this.video.style.cursor = "";
  }

  /** Where `e` falls on the picture (object-fit: contain), 0..1. */
  private position(e: MouseEvent): { x: number; y: number } {
    const r = this.video.getBoundingClientRect();
    const vw = this.video.videoWidth || r.width;
    const vh = this.video.videoHeight || r.height;
    const scale = Math.min(r.width / vw, r.height / vh);
    const w = vw * scale;
    const h = vh * scale;
    return { x: (e.clientX - r.left - (r.width - w) / 2) / w, y: (e.clientY - r.top - (r.height - h) / 2) / h };
  }

  private move(e: PointerEvent): void {
    if (this.locked) {
      // Movement is in CSS pixels; the remote pointer moves in stream pixels.
      const r = this.video.getBoundingClientRect();
      const scale = (this.video.videoWidth || r.width) / r.width;
      if (e.movementX || e.movementY) this.send({ k: "rel", dx: e.movementX * scale, dy: e.movementY * scale });
    } else {
      this.send({ k: "move", ...this.position(e) });
    }
  }

  private button(e: PointerEvent, down: boolean): void {
    if (down) {
      this.video.focus();
      if (!this.locked) {
        this.video.setPointerCapture(e.pointerId);
        this.send({ k: "move", ...this.position(e) });
      }
      this.buttons.add(e.button);
    } else {
      this.buttons.delete(e.button);
    }
    this.send({ k: "button", b: e.button, down });
    e.preventDefault();
  }

  private wheel(e: WheelEvent): void {
    // deltaMode 1 (lines) is rare on desktops; treat a line as ~40 px.
    const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
    this.send({ k: "wheel", dx: e.deltaX * unit, dy: e.deltaY * unit });
    e.preventDefault();
  }

  private key(e: KeyboardEvent, down: boolean): void {
    e.preventDefault();
    if (e.repeat) return; // the app repeats held keys itself
    const code = e.code || codeForKey(e.key);
    if (!code) return;
    if (down) this.keys.add(code);
    else this.keys.delete(code);
    this.send({ k: "key", code, down });
  }

  private releaseAll(): void {
    for (const code of this.keys) this.send({ k: "key", code, down: false });
    for (const b of this.buttons) this.send({ k: "button", b, down: false });
    this.keys.clear();
    this.buttons.clear();
  }
}

const PUNCTUATION: Record<string, string> = {
  " ": "Space",
  "-": "Minus",
  "=": "Equal",
  "[": "BracketLeft",
  "]": "BracketRight",
  ";": "Semicolon",
  "'": "Quote",
  "`": "Backquote",
  "\\": "Backslash",
  ",": "Comma",
  ".": "Period",
  "/": "Slash",
};

/**
 * A physical key for events that carry none (on-screen keyboards, some input
 * methods, automation): unshifted US-layout characters only. Full text input
 * (IME, other layouts) comes with the text-input protocol (plan §3.5).
 */
export function codeForKey(key: string): string | null {
  if (key.length !== 1) return ["Enter", "Backspace", "Tab", "Escape"].includes(key) ? key : null;
  if (/^[a-z]$/.test(key)) return `Key${key.toUpperCase()}`;
  if (/^[0-9]$/.test(key)) return `Digit${key}`;
  return PUNCTUATION[key] ?? null;
}
