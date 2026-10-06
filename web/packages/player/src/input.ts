// Keyboard and mouse from the <video> element to the environment, on the
// control channel. Two pointer modes:
// - absolute (desktops): positions normalized to the picture;
// - locked (games): raw relative motion, scaled to stream pixels.
// Keys go by `KeyboardEvent.code` (the physical key); the streamer maps them.
//
// Pasting: on Ctrl/Cmd+V the V is held back and the browser's own paste
// event (no permission prompt) supplies the local clipboard, which goes up
// first, so the app pastes what was copied here.

import { CaptureMode, type CaptureView } from "./captureMode";

type Send = (msg: Record<string, unknown>) => void;

export interface InputOptions {
  /** The local clipboard's text, just before a paste shortcut reaches the app. */
  onPaste?: (text: string) => void;
  /** Send ⌘ as Ctrl, so the Mac's shortcuts work in Linux apps (default: on Macs). */
  commandAsControl?: boolean;
  /** Mouse capture changed: captured, released by the browser (recapture mode), or off. */
  onCapture?: (view: CaptureView) => void;
}

/** How long a paste shortcut waits for the browser's paste event. */
const PASTE_WAIT_MS = 150;
/** How long the recapture hint stays without any interaction. */
const HINT_MS = 4000;
const IS_MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export class InputCapture {
  private readonly keys = new Set<string>();
  private readonly buttons = new Set<number>();
  private readonly cleanup: (() => void)[] = [];
  private readonly commandAsControl: boolean;
  /** Keys pressed while ⌘ was down: macOS never sends their keyups. */
  private readonly commandChord = new Set<string>();
  /** A paste shortcut's key, held until the paste event (or a timeout). */
  private readonly capture = new CaptureMode();
  private hintTimer: ReturnType<typeof setTimeout> | null = null;
  private keyboardLock = false;
  /** Off: the mouse (moves, buttons, wheel) is not sent; the keyboard still is. */
  private mouseOn = true;
  private heldPaste: { code: string; timer: ReturnType<typeof setTimeout> } | null = null;

  constructor(
    private readonly video: HTMLVideoElement,
    private readonly send: Send,
    private readonly options: InputOptions = {},
  ) {
    this.commandAsControl = options.commandAsControl ?? IS_MAC;
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
    on(document, "paste", (e: ClipboardEvent) => this.paste(e));
    // Losing focus mid-press would leave keys and buttons stuck down remotely.
    on(video, "blur", () => this.releaseAll());
    on(window, "blur", () => this.releaseAll());
    on(document, "pointerlockchange", () => {
      this.releaseAll();
      this.lockChanged();
    });
    on(document, "fullscreenchange", () => this.syncKeyboardLock());
  }

  /** Turn the mouse's input to the environment off or on. Off lets go of held buttons and of a captured pointer. */
  setMouseEnabled(on: boolean): void {
    if (on === this.mouseOn) return;
    this.mouseOn = on;
    if (on) return;
    for (const b of this.buttons) this.send({ k: "button", b, down: false });
    this.buttons.clear();
    if (this.locked) document.exitPointerLock();
  }

  get mouseEnabled(): boolean {
    return this.mouseOn;
  }

  /** Mouse capture, for the UI. */
  get captureView(): CaptureView {
    return this.capture.view;
  }

  /** Every key, Esc and browser shortcuts included, goes to the stream (Chromium, full screen, captured). */
  get keyboardLocked(): boolean {
    return this.keyboardLock;
  }

  /** Leave recapture mode: clicks go to the stream again, with the client-side cursor. */
  turnOffCapture(): void {
    this.capture.turnOff();
    this.clearHintTimer();
    this.emitCapture();
  }

  /** Hide the hint now (the user acted on it). */
  hideHint(): void {
    this.capture.hideHint();
    this.clearHintTimer();
    this.emitCapture();
  }

  private emitCapture(): void {
    this.options.onCapture?.(this.capture.view);
  }

  private clearHintTimer(): void {
    if (this.hintTimer) clearTimeout(this.hintTimer);
    this.hintTimer = null;
  }

  /** (Re)start the hint's countdown; it is only shown while released. */
  private armHint(): void {
    this.clearHintTimer();
    if (!this.capture.view.hint) return;
    this.hintTimer = setTimeout(() => {
      this.hintTimer = null;
      this.capture.hideHint();
      this.emitCapture();
    }, HINT_MS);
  }

  private lockChanged(): void {
    if (this.locked) this.capture.locked();
    else this.capture.unlocked();
    this.armHint();
    this.emitCapture();
    this.syncKeyboardLock();
  }

  /** Keyboard Lock needs full screen, and we want it only while the mouse is captured too. */
  private syncKeyboardLock(): void {
    const kb = (navigator as Navigator & { keyboard?: { lock?: (codes?: string[]) => Promise<void>; unlock?: () => void } })
      .keyboard;
    if (!kb?.lock) return;
    const want = !!document.fullscreenElement && this.locked;
    if (want === this.keyboardLock) return;
    this.keyboardLock = want;
    if (want) {
      kb.lock().catch(() => {
        this.keyboardLock = false;
      });
    } else {
      kb.unlock?.();
    }
  }

  /** A lock request is in flight. */
  private locking = false;

  get locked(): boolean {
    return document.pointerLockElement === this.video;
  }

  /**
   * Raw mouse for games: the pointer disappears and moves relatively. One lock at a time: Safari
   * has no raw (unadjusted) movement, and asking with it and then again without left two locks,
   * one per Esc, the first with no cursor anywhere.
   */
  async lockPointer(): Promise<void> {
    if (!this.mouseOn || this.locked || this.locking) return;
    this.locking = true;
    // Keys go to the stream while the mouse is captured, not to the button that asked.
    this.video.focus();
    try {
      // Raw movement is Chromium's (Chrome, Edge); elsewhere ask plainly, once.
      if (!("userAgentData" in navigator)) {
        await this.video.requestPointerLock();
        return;
      }
      try {
        await this.video.requestPointerLock({ unadjustedMovement: true });
      } catch {
        // No raw input here (e.g. Chrome on Linux): accelerated is still usable, unless the
        // first request locked after all.
        if (!this.locked) await this.video.requestPointerLock();
      }
    } finally {
      this.locking = false;
    }
  }

  dispose(): void {
    this.clearHintTimer();
    if (this.keyboardLock) {
      this.keyboardLock = false;
      (navigator as Navigator & { keyboard?: { unlock?: () => void } }).keyboard?.unlock?.();
    }
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
    if (!this.mouseOn) return;
    // Moving over the hint is interaction: it stays.
    if (this.capture.view.hint) this.armHint();
    if (this.locked) {
      // Movement is in CSS pixels; the remote pointer moves in stream pixels.
      // The picture is letterboxed (object-fit: contain), so scale by its
      // drawn size, not the element's.
      const r = this.video.getBoundingClientRect();
      const vw = this.video.videoWidth || r.width;
      const vh = this.video.videoHeight || r.height;
      const scale = 1 / Math.min(r.width / vw, r.height / vh);
      if (e.movementX || e.movementY) this.send({ k: "rel", dx: e.movementX * scale, dy: e.movementY * scale });
    } else {
      this.send({ k: "move", ...this.position(e) });
    }
  }

  private button(e: PointerEvent, down: boolean): void {
    if (!this.mouseOn) {
      // Clicking the picture still gives it the keyboard.
      if (down) this.video.focus();
      return;
    }
    if (!this.locked) {
      // After the browser took the mouse back, a click captures it again; it is not the app's.
      if (down && this.capture.pointerDown(e.button) === "recapture") {
        e.preventDefault();
        void this.lockPointer();
        return;
      }
      if (!down && this.capture.pointerUp(e.button) === "swallow") {
        e.preventDefault();
        return;
      }
    }
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
    if (!this.mouseOn) return;
    // deltaMode 1 (lines) is rare on desktops; treat a line as ~40 px.
    const unit = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1;
    this.send({ k: "wheel", dx: e.deltaX * unit, dy: e.deltaY * unit });
    e.preventDefault();
  }

  private key(e: KeyboardEvent, down: boolean): void {
    let code = e.code || codeForKey(e.key);
    if (!code) {
      e.preventDefault();
      return;
    }
    const command = code === "MetaLeft" || code === "MetaRight";
    if (command && !down) this.releaseChord();
    else if (!command && down && e.metaKey && IS_MAC) this.commandChord.add(code);
    if (this.commandAsControl && command) code = code === "MetaLeft" ? "ControlLeft" : "ControlRight";
    const pasting = down && code === "KeyV" && (e.ctrlKey || e.metaKey) && !e.altKey && this.options.onPaste;
    // Let the browser fire its paste event; everything else stays ours.
    if (!pasting) e.preventDefault();
    if (e.repeat) return; // the app repeats held keys itself
    if (pasting) {
      if (this.heldPaste) return;
      this.keys.add(code);
      this.heldPaste = { code, timer: setTimeout(() => this.releasePaste(), PASTE_WAIT_MS) };
      return;
    }
    if (!down && code === this.heldPaste?.code) this.releasePaste();
    if (down) this.keys.add(code);
    else this.keys.delete(code);
    this.send({ k: "key", code, down });
  }

  /** The browser's paste: its text goes up before the held V. */
  private paste(e: ClipboardEvent): void {
    if (!this.heldPaste) return;
    e.preventDefault();
    const text = e.clipboardData?.getData("text/plain");
    if (text) this.options.onPaste?.(text);
    this.releasePaste();
  }

  /** ⌘ came up: so did the keys pressed with it, whatever macOS reported. */
  private releaseChord(): void {
    for (const code of this.commandChord) {
      if (code === this.heldPaste?.code) this.releasePaste();
      if (this.keys.delete(code)) this.send({ k: "key", code, down: false });
    }
    this.commandChord.clear();
  }

  private releasePaste(): void {
    const held = this.heldPaste;
    if (!held) return;
    this.heldPaste = null;
    clearTimeout(held.timer);
    this.send({ k: "key", code: held.code, down: true });
  }

  private releaseAll(): void {
    this.releasePaste();
    this.commandChord.clear();
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
