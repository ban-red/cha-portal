// A dev-only input harness for testing: drives the <video> with the same
// keyboard and pointer events a person's devices produce, so `InputCapture`
// (and everything behind it: control channel, streamer, the app) runs its
// normal path. Reach it as `window.__chaHarness` from the console or a browser
// tool. It is installed only in dev builds and makes no attempt to hide
// that it is automated; it is for latency and input-handling tests against
// our own apps, where repeatable, human-shaped timing matters.
//
// The human shape, all of it from one seeded generator (the same seed replays
// the same run): dwell times and gaps are log-normal around a nominal value,
// hesitations included; pointer moves either track smoothly along a
// minimum-jerk path or flick — ballistic, overshooting, then corrected — with
// the duration taken from Fitts's law when a call leaves it out; the wheel
// scrolls in physical notches; held keys auto-repeat like an OS keyboard. Key
// events carry real `key` values and live modifier flags, so shortcuts
// (Ctrl+V) and shifted keys behave like the real thing.
//
// Setup. A session id alone is not enough: the harness lives in the page of a
// running session, in a browser, not in the portal or the node.
//   1. A dev portal is up (`bun run dev`; the SPA is on :7678).
//   2. Open the session page in a browser you can run JavaScript in
//      (DevTools, or a browser tool's javascript_tool), signed in:
//      http://localhost:7678/environments/<environment id>/session
//      The environment id (the session id in that URL) picks which environment
//      the page streams; the harness drives whichever page you ran it in.
//   3. Wait for the picture. The harness appears when the control channel opens
//      (check `window.__chaHarness`), and only on a dev build (`import.meta.env.DEV`);
//      a production build never has it. You need the controls (owner/admin).
//   4. Click the picture once. That gives the video the keyboard and, for games,
//      captures the pointer; synthetic events cannot take pointer lock, so
//      `look` turns a game's camera only after that real click.
//
// Calls are async: `await` each, or chain them in one script, since a
// browser tool's one-key-at-a-time actions cannot hold a key.
//
//   const h = window.__chaHarness
//   h.seed(7)                       // the same seed replays the same run
//   await h.hold("KeyW", 1500)      // run forward (auto-repeat included)
//   await h.look(240, -40)          // flick the camera; ~ms from Fitts's law
//   await h.moveTo(0.5, 0.5, 600)   // absolute pointer, as a fraction of the picture
//   await h.type("Hello, world")    // digraph-paced typing, Shift where needed
//   await h.scroll(-360)            // the wheel, in human notches
//   await h.chord(["ControlLeft", "KeyV"], 60) // a shortcut, with real modifier flags
//   await h.tap("Space")
//   await h.padStick({ lx: -1 }, 1200)   // left stick: a thumb-shaped tilt, spring release
//   await h.padTap("a", 3)               // mash a button, tiring
//   await h.padPull("rt", 300)           // an analog trigger, pulled like a finger
//   h.releaseAll()                  // let go of everything between phases
//
// Reading the picture: `frame()` draws the current video frame into a small
// reused canvas (the downscale happens on the GPU) and reads back a few KB, so
// it is cheap enough to call every frame. `waitFor` checks on presented
// frames (requestVideoFrameCallback), not on a timer.
//
//   const f = h.frame()                          // 64×36 RGBA by default
//   h.mean(f, { x: 0.4, y: 0.4, w: 0.2, h: 0.2 })  // average colour of a region
//   h.grid(8)                                    // a colour grid: a few hundred bytes, not a frame
//   await h.waitFor((f) => h.mean(f).r > 200, 5000)
//   await h.waitChange(5000)                     // until the picture moves
//   await h.png(256, { x: 0.3, y: 0.3, w: 0.4, h: 0.4 })  // a crop as PNG, to look at
//   const aim = h.servo({ match: { channel: "r", min: 150 } })    // hold a red target centred
//   aim.stop()
//
// First-person cameras: `look` sends the whole-pixel relative deltas a real
// mouse does, but only while the mouse is captured (`h.locked`). The one
// setup click captures it; after any unlock (a held Esc, the browser taking
// the mouse back) only another real click captures it again, so assert
// `h.locked` before a camera phase. Keep Escape out of scripted keys while
// captured: holding it is the let-go gesture.
//
// Controller: the pad goes up as the session's own pad (slot 0), the same
// change-only, three-decimal state messages a real pad produces — so a real
// pad on the same page would fight it; unplug one or the other. Sticks move
// like a thumb does (rise, tremor, spring release with overshoot), triggers
// pull like a finger, mashes tire, and everything cancels on releaseAll.

import { pictureSize } from "./crop";
import { round3 } from "./controllers/types";

/** A small seeded generator (mulberry32): the same seed gives the same run. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * A log-normal sample around `median`: human dwell times and gaps are skewed
 * this way, with a heavy tail (the occasional 2–3× hesitation) but no negative
 * values. The multiplier is clamped to [⅓, 3] so a test run stays sane.
 */
export function lognormal(rand: () => number, median: number, sigma = 0.25): number {
  const u = Math.max(rand(), 1e-9);
  const g = Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * rand());
  const m = Math.min(3, Math.max(1 / 3, Math.exp(sigma * g)));
  return median * m;
}

/** A minimum-jerk profile: 0 at t=0, 1 at t=1, slow at both ends. */
export function minJerk(t: number): number {
  const c = Math.min(1, Math.max(0, t));
  return c ** 3 * (10 - 15 * c + 6 * c * c);
}

/**
 * The per-step offsets of a pointer move of (dx, dy) over `steps` steps, along a minimum-jerk
 * profile with a slight sideways bow of `bow` (a fraction of the distance). The steps sum to (dx, dy).
 */
export function pathSteps(dx: number, dy: number, steps: number, bow = 0): [number, number][] {
  const n = Math.max(1, Math.round(steps));
  const len = Math.hypot(dx, dy) || 1;
  // Unit normal to the move, for the bow.
  const nx = -dy / len;
  const ny = dx / len;
  const out: [number, number][] = [];
  let px = 0;
  let py = 0;
  for (let i = 1; i <= n; i++) {
    const s = minJerk(i / n);
    const side = Math.sin(Math.PI * (i / n)) * bow * len;
    const tx = i === n ? dx : dx * s + nx * side;
    const ty = i === n ? dy : dy * s + ny * side;
    out.push([tx - px, ty - py]);
    px = tx;
    py = ty;
  }
  return out;
}

/**
 * Movement time by Fitts's law: ~170 ms to start with, plus a logarithmic
 * distance term against a target `width` of ~80 px. Good enough to keep
 * scripts from hand-picking durations.
 */
export function fittsMs(distance: number, width = 80): number {
  return 170 + 130 * Math.log2(1 + Math.max(0, distance) / width);
}

/** A wheel delta split into physical-notch-sized steps (≤ ~121 px), sign kept. */
export function wheelNotches(total: number): number[] {
  const a = Math.abs(total);
  if (a === 0) return [];
  const n = Math.min(60, Math.max(1, Math.ceil(a / 120)));
  const base = Math.trunc(total / n);
  const out: number[] = Array.from({ length: n }, () => base);
  out[0] = total - base * (n - 1);
  return out;
}

/**
 * A nominal inter-key interval in ms: a repeated key is slowest (same finger),
 * a likely hand alternation (vowel/consonant) quickest, same-class pairs in
 * between; anything crossing a word boundary sits apart from all of them.
 */
export function typeDelay(prev: string, next: string): number {
  if (prev.toLowerCase() === next.toLowerCase()) return 190;
  if (!/^[a-z]$/i.test(prev) || !/^[a-z]$/i.test(next)) return 150;
  const vowel = (c: string) => "aeiou".includes(c.toLowerCase());
  return vowel(prev) !== vowel(next) ? 85 : 130;
}

/** Intermediate values from `from` to `to` along the minimum-jerk profile; the last is exact. */
export function padRamp(from: number, to: number, steps: number): number[] {
  const n = Math.max(2, Math.round(steps));
  const out: number[] = [];
  for (let i = 1; i <= n; i++) out.push(i === n ? to : from + (to - from) * minJerk(i / n));
  return out;
}

const KEY_VALUES: Record<string, string> = {
  Space: " ",
  Enter: "Enter",
  Backspace: "Backspace",
  Tab: "Tab",
  Escape: "Escape",
  CapsLock: "CapsLock",
  ArrowUp: "ArrowUp",
  ArrowDown: "ArrowDown",
  ArrowLeft: "ArrowLeft",
  ArrowRight: "ArrowRight",
  ShiftLeft: "Shift",
  ShiftRight: "Shift",
  ControlLeft: "Control",
  ControlRight: "Control",
  AltLeft: "Alt",
  AltRight: "Alt",
  MetaLeft: "Meta",
  MetaRight: "Meta",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
};

/** What Shift turns these codes into on a US layout. */
const SHIFTED: Record<string, string> = {
  Digit1: "!",
  Digit2: "@",
  Digit3: "#",
  Digit4: "$",
  Digit5: "%",
  Digit6: "^",
  Digit7: "&",
  Digit8: "*",
  Digit9: "(",
  Digit0: ")",
  Backquote: "~",
  Minus: "_",
  Equal: "+",
  BracketLeft: "{",
  BracketRight: "}",
  Backslash: "|",
  Semicolon: ":",
  Quote: '"',
  Comma: "<",
  Period: ">",
  Slash: "?",
};

/** The `KeyboardEvent.key` a real keyboard sends for `code` (shift-aware); unknown codes pass through. */
export function keyFor(code: string, shift = false): string {
  if (shift && SHIFTED[code]) return SHIFTED[code]!;
  if (/^Key[A-Z]$/.test(code)) return shift ? code[3]! : code[3]!.toLowerCase();
  if (/^Digit[0-9]$/.test(code)) return code[5]!;
  if (/^F([1-9]|1[0-2])$/.test(code)) return code;
  return KEY_VALUES[code] ?? code;
}

const CHAR_CODES: Record<string, string> = {
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

/** The shifted punctuation a US layout produces, with the key that carries it. */
const SHIFT_CHARS: Record<string, [string, boolean]> = {
  "!": ["Digit1", true],
  "@": ["Digit2", true],
  "#": ["Digit3", true],
  $: ["Digit4", true],
  "%": ["Digit5", true],
  "^": ["Digit6", true],
  "&": ["Digit7", true],
  "*": ["Digit8", true],
  "(": ["Digit9", true],
  ")": ["Digit0", true],
  _: ["Minus", true],
  "+": ["Equal", true],
  "{": ["BracketLeft", true],
  "}": ["BracketRight", true],
  "|": ["Backslash", true],
  ":": ["Semicolon", true],
  '"': ["Quote", true],
  "~": ["Backquote", true],
  "<": ["Comma", true],
  ">": ["Period", true],
  "?": ["Slash", true],
};

/** The physical key and Shift state that produces `ch` on a US layout; null when we cannot send it. */
export function codeForChar(ch: string): [string, boolean] | null {
  if (/^[a-z]$/.test(ch)) return [`Key${ch.toUpperCase()}`, false];
  if (/^[A-Z]$/.test(ch)) return [`Key${ch}`, true];
  if (/^[0-9]$/.test(ch)) return [`Digit${ch}`, false];
  if (ch in CHAR_CODES) return [CHAR_CODES[ch]!, false];
  return SHIFT_CHARS[ch] ?? null;
}

/** A downscaled frame: RGBA bytes, `w`×`h`. */
export interface Frame {
  w: number;
  h: number;
  data: Uint8ClampedArray;
}

/** A region of a frame, as fractions of its width and height. */
export interface Region {
  x: number;
  y: number;
  w: number;
  h: number;
}

const WHOLE: Region = { x: 0, y: 0, w: 1, h: 1 };

/** A region's pixel bounds in a frame. */
function regionBounds(f: Frame, r: Region): { x0: number; y0: number; x1: number; y1: number } {
  const x0 = Math.max(0, Math.floor(r.x * f.w));
  const y0 = Math.max(0, Math.floor(r.y * f.h));
  const x1 = Math.min(f.w, Math.max(x0 + 1, Math.ceil((r.x + r.w) * f.w)));
  const y1 = Math.min(f.h, Math.max(y0 + 1, Math.ceil((r.y + r.h) * f.h)));
  return { x0, y0, x1, y1 };
}

/** The average colour of a region (the whole frame by default). */
export function meanRgb(f: Frame, r: Region = WHOLE): { r: number; g: number; b: number } {
  const { x0, y0, x1, y1 } = regionBounds(f, r);
  let sr = 0;
  let sg = 0;
  let sb = 0;
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      const i = (y * f.w + x) * 4;
      sr += f.data[i]!;
      sg += f.data[i + 1]!;
      sb += f.data[i + 2]!;
    }
  }
  const n = (x1 - x0) * (y1 - y0);
  return { r: sr / n, g: sg / n, b: sb / n };
}

/** What a target looks like: one colour channel within a range. */
export interface ChannelMatch {
  channel: "r" | "g" | "b";
  min?: number;
  max?: number;
}

const inMatch = (v: number, m: ChannelMatch): boolean => v >= (m.min ?? 0) && v <= (m.max ?? 255);

/** The fraction of a region's pixels whose channel sits within the match, 0..1. */
export function matchFraction(f: Frame, r: Region, m: ChannelMatch): number {
  const { x0, y0, x1, y1 } = regionBounds(f, r);
  const c = m.channel === "r" ? 0 : m.channel === "g" ? 1 : 2;
  let n = 0;
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      if (inMatch(f.data[(y * f.w + x) * 4 + c]!, m)) n++;
    }
  }
  return n / ((x1 - x0) * (y1 - y0));
}

/** Where the matching pixels sit, as fractions of the whole frame; null when nothing matches. */
export function centroidOf(f: Frame, r: Region, m: ChannelMatch): { x: number; y: number; n: number } | null {
  const { x0, y0, x1, y1 } = regionBounds(f, r);
  const c = m.channel === "r" ? 0 : m.channel === "g" ? 1 : 2;
  let n = 0;
  let sx = 0;
  let sy = 0;
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      if (inMatch(f.data[(y * f.w + x) * 4 + c]!, m)) {
        n++;
        sx += x;
        sy += y;
      }
    }
  }
  if (n === 0) return null;
  return { x: sx / n / f.w, y: sy / n / f.h, n };
}

/** A colour grid over the picture: `cols` cells across, rows to keep the aspect, row-major. */
export interface Grid {
  cols: number;
  rows: number;
  cells: { r: number; g: number; b: number }[];
}

export function gridOf(f: Frame, cols: number): Grid {
  const c = Math.max(1, Math.round(cols));
  const rows = Math.max(1, Math.round((c * f.h) / f.w));
  const cells = [];
  for (let ry = 0; ry < rows; ry++) {
    for (let cx = 0; cx < c; cx++) {
      cells.push(meanRgb(f, { x: cx / c, y: ry / rows, w: 1 / c, h: 1 / rows }));
    }
  }
  return { cols: c, rows, cells };
}

/** A servo policy: keep a colour target centred with small camera corrections. */
export interface TrackPolicy {
  match: ChannelMatch;
  /** Where to search for it (the whole picture by default). */
  region?: Region;
  /** Look pixels per fraction of error across the picture (default 0.7). */
  gain?: number;
  /** Errors inside this fraction of the picture are left alone (default 0.01). */
  deadzone?: number;
  /** Fewer matching pixels than this means the target is gone (default 4). */
  minPixels?: number;
  /** Frame width the centroid reads from (default 96). */
  width?: number;
  /** Correct on every n-th presented frame; the stream lags a frame or two (default 3). */
  every?: number;
  /** Each correction's duration in ms (default 14). */
  tickMs?: number;
}

export interface ServoResult {
  stopped: "lost" | "timeout" | "manual" | "unlocked";
  frames: number;
  ms: number;
  /** Where the target last sat, as fractions of the picture; null if it was never seen. */
  error: { x: number; y: number } | null;
}

export interface ServoRun {
  result: Promise<ServoResult>;
  stop(): void;
}

/** The mean absolute difference between two frames of one size, 0 (same) to 255, on green (cheap luma). */
export function frameDiff(a: Frame, b: Frame): number {
  if (a.w !== b.w || a.h !== b.h) return 255;
  let sum = 0;
  for (let i = 1; i < a.data.length; i += 4) sum += Math.abs(a.data[i]! - b.data[i]!);
  return sum / (a.data.length / 4);
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The pad's buttons by name, as the standard mapping (an XInput pad's layout): indices into `b`. */
export const PAD_BUTTONS = {
  a: 0, b: 1, x: 2, y: 3, lb: 4, rb: 5, lt: 6, rt: 7, back: 8, start: 9,
  ls: 10, rs: 11, up: 12, down: 13, left: 14, right: 15, guide: 16,
} as const;
/** The sticks by name, as indices into `a`. */
export const PAD_AXES = { lx: 0, ly: 1, rx: 2, ry: 3 } as const;

const clampTo = (v: unknown, lo: number, hi: number): number =>
  Math.min(hi, Math.max(lo, typeof v === "number" && Number.isFinite(v) ? v : 0));

const MOD_CODES = {
  ControlLeft: "ctrl",
  ControlRight: "ctrl",
  AltLeft: "alt",
  AltRight: "alt",
  ShiftLeft: "shift",
  ShiftRight: "shift",
  MetaLeft: "meta",
  MetaRight: "meta",
} as const;

export class Harness {
  private rand = rng(1);
  private x: number;
  private y: number;
  /** Live modifier flags, stamped onto every key event like a real keyboard's. */
  private readonly mods = { ctrl: false, alt: false, shift: false, meta: false };
  /** What the harness is currently holding down, so a broken script can let go. */
  private readonly held = new Set<string>();
  private readonly clicking = new Set<number>();
  /** The un-sent fractional part of pointer moves (see `pointer`). */
  private readonly rest = { x: 0, y: 0 };

  /** `sendPad` carries a pad message to the session (the player's send path, with its control gate). */
  constructor(
    private readonly video: HTMLVideoElement,
    private readonly sendPad?: (msg: Record<string, unknown>) => void,
  ) {
    const r = video.getBoundingClientRect();
    this.x = r.left + r.width / 2;
    this.y = r.top + r.height / 2;
  }

  /** The pad the harness holds, as the streamer reads it: 24 buttons, 4 axes. Neutral until a pad run. */
  private pad = { b: new Array<number>(24).fill(0), a: [0, 0, 0, 0] };
  /** The quantized key of what was last sent, so the pad streams change-only like the real ones. */
  private padLast = "";
  /** Live runs per pad index ("b6", "a0"): overlapping holds let go only of their own. */
  private readonly padRefs = new Map<string, number>();
  /** Bumped by releaseAll: a pad run whose generation is stale stops where it is, sending nothing. */
  private padGen = 0;

  private padIdle(): boolean {
    return this.pad.b.every((v) => v === 0) && this.pad.a.every((v) => v === 0);
  }

  /** Whether a pad run is holding anything right now. */
  get padHeld(): boolean {
    return !this.padIdle();
  }

  /** Send the held pad as player pad message 0 (the session's own pad), typed as an XInput pad. */
  private padSend(): void {
    if (!this.sendPad) throw new Error("no pad channel on this page");
    const key = `${this.pad.b.map(round3).join(",")}|${this.pad.a.map(round3).join(",")}`;
    if (key === this.padLast) return;
    this.padLast = key;
    this.sendPad({ k: "pad", i: 0, ty: "xbox", b: this.pad.b.map(round3), a: this.pad.a.map(round3) });
  }

  private padButtons(spec: Record<string, unknown>): [number, number][] {
    return Object.keys(spec).map((n) => {
      const i = PAD_BUTTONS[n as keyof typeof PAD_BUTTONS];
      if (i === undefined) throw new Error(`unknown pad button: ${n} (use ${Object.keys(PAD_BUTTONS).join(", ")})`);
      return [i, clampTo(spec[n], 0, 1)] as const as [number, number];
    });
  }

  private padAxes(spec: Record<string, unknown>): [number, number][] {
    return Object.keys(spec).map((n) => {
      const i = PAD_AXES[n as keyof typeof PAD_AXES];
      if (i === undefined) throw new Error(`unknown pad axis: ${n} (use lx, ly, rx, ry)`);
      return [i, clampTo(spec[n], -1, 1)] as const as [number, number];
    });
  }

  /** Take and later give back pad indices, so overlapping runs never let go of each other's. */
  private padTake(refs: string[]): void {
    for (const r of refs) this.padRefs.set(r, (this.padRefs.get(r) ?? 0) + 1);
  }

  private padGive(refs: string[]): void {
    for (const r of refs) {
      const left = (this.padRefs.get(r) ?? 1) - 1;
      if (left > 0) {
        this.padRefs.set(r, left);
        continue;
      }
      this.padRefs.delete(r);
      if (r[0] === "b") this.pad.b[Number(r.slice(1))] = 0;
      else this.pad.a[Number(r.slice(1))] = 0;
    }
  }

  /** A centred normal sample (Box–Muller), for a thumb's tremor and such. */
  private gauss(sigma = 1): number {
    const u = Math.max(this.rand(), 1e-9);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * this.rand()) * sigma;
  }

  /**
   * Hold an XInput-style pad for `ms`, then let go — exactly, with no shaping, for tests that
   * want a known state. `buttons` maps names from PAD_BUTTONS to 0..1 (triggers are analog);
   * `axes` maps lx, ly, rx, ry to -1..1. Unknown names throw before anything is sent. Overlapping
   * holds keep their own indices: letting one go never drops another's.
   */
  async padHold(buttons: Record<string, unknown>, axes: Record<string, unknown>, ms: number): Promise<void> {
    const bi = this.padButtons(buttons);
    const ai = this.padAxes(axes);
    const refs = [...bi.map(([i]) => `b${i}`), ...ai.map(([i]) => `a${i}`)];
    this.padTake(refs);
    try {
      for (const [i, v] of bi) this.pad.b[i] = v;
      for (const [i, v] of ai) this.pad.a[i] = v;
      this.padSend();
      await sleep(Math.max(0, ms));
    } finally {
      this.padGive(refs);
      this.padSend();
    }
  }

  /**
   * Move sticks like a thumb does: a rise to the target over ~95 ms (~45 for a flick), a hold
   * with small tremor, then the spring carries the stick past centre and it settles — a flick's
   * counter-pull is that overshoot, bigger to cancel the swing. `ms` is the hold after the
   * rise. Updates go only when the quantized state changes, like a real pad's stream.
   */
  async padStick(axes: Record<string, unknown>, ms: number, style: "tilt" | "flick" = "tilt"): Promise<void> {
    const targets = this.padAxes(axes);
    if (targets.length === 0) throw new Error("pad_stick needs axes, e.g. { lx: -0.8 }");
    const refs = targets.map(([i]) => `a${i}`);
    const gen = this.padGen;
    this.padTake(refs);
    const bail = () => this.padGen !== gen;
    try {
      const start = performance.now();
      const peak = targets.map(([, v]) => v);
      // Rise.
      const rise = this.around(style === "flick" ? 45 : 95, 0.3);
      const up = targets.map(([i, v]) => padRamp(this.pad.a[i]!, v, Math.max(2, Math.round(rise / 32))));
      for (let s = 0; s < up[0]!.length; s++) {
        await this.until(start, ((s + 1) * rise) / up[0]!.length);
        if (bail()) return;
        targets.forEach(([i], k) => (this.pad.a[i] = up[k]![s]!));
        this.padSend();
      }
      // Hold with tremor, quiet between the small updates.
      const holdMs = this.around(Math.max(0, ms));
      let at = rise + this.around(70, 0.5);
      while (at < rise + holdMs - 30) {
        await this.until(start, at);
        if (bail()) return;
        for (const [i, v] of targets) this.pad.a[i] = clampTo(v + this.gauss(0.015), -1, 1);
        this.padSend();
        at += this.around(80, 0.5);
      }
      await this.until(start, rise + holdMs);
      if (bail()) return;
      // Release: past centre, then settle.
      const over = style === "flick" ? 0.14 : 0.08;
      const back = this.around(70, 0.35);
      const down = targets.map(([, v]) => padRamp(v, -v * over, Math.max(2, Math.round(back / 32))));
      for (let s = 0; s < down[0]!.length; s++) {
        await this.until(start, rise + holdMs + ((s + 1) * back) / down[0]!.length);
        if (bail()) return;
        targets.forEach(([i], k) => (this.pad.a[i] = down[k]![s]!));
        this.padSend();
      }
      // Two paced steps from the overshoot back to centre.
      const relStart = rise + holdMs + back;
      const rel = this.around(60, 0.35);
      for (let s = 0; s < 2; s++) {
        await this.until(start, relStart + ((s + 1) * rel) / 2);
        if (bail()) return;
        targets.forEach(([i], k) => (this.pad.a[i] = s === 0 ? -peak[k]! * over : 0));
        this.padSend();
      }
    } finally {
      this.padGive(refs);
      if (this.sendPad) this.padSend();
    }
  }

  /** Press and release a pad button `count` times, like a thumb mash: ~7 presses/s, tiring. */
  async padTap(button: string, count = 1): Promise<void> {
    const i = PAD_BUTTONS[button as keyof typeof PAD_BUTTONS];
    if (i === undefined) throw new Error(`unknown pad button: ${button} (use ${Object.keys(PAD_BUTTONS).join(", ")})`);
    const refs = [`b${i}`];
    const gen = this.padGen;
    this.padTake(refs);
    try {
      const n = clampTo(count, 1, 32);
      for (let k = 0; k < n; k++) {
        this.pad.b[i] = 1;
        this.padSend();
        await sleep(this.around(85, 0.35));
        if (this.padGen !== gen) return;
        this.pad.b[i] = 0;
        this.padSend();
        if (k < n - 1) await sleep(this.around(Math.max(4, 1000 / (7 - k * 0.5)), 0.3));
      }
    } finally {
      this.padGive(refs);
      if (this.sendPad) this.padSend();
    }
  }

  /** Pull an analog trigger to `depth` (default 1) over ~70 ms, hold for `ms`, ease off over ~90 ms. */
  async padPull(trigger: "lt" | "rt", ms: number, depth = 1): Promise<void> {
    const i = PAD_BUTTONS[trigger];
    if (i === undefined) throw new Error("pad_pull takes lt or rt");
    const refs = [`b${i}`];
    const gen = this.padGen;
    this.padTake(refs);
    const to = clampTo(depth, 0, 1);
    try {
      const start = performance.now();
      const rise = this.around(70, 0.3);
      const up = padRamp(0, to, Math.max(2, Math.round(rise / 32)));
      for (let s = 0; s < up.length; s++) {
        await this.until(start, ((s + 1) * rise) / up.length);
        if (this.padGen !== gen) return;
        this.pad.b[i] = up[s]!;
        this.padSend();
      }
      const holdEnd = rise + this.around(Math.max(0, ms));
      await this.until(start, holdEnd);
      if (this.padGen !== gen) return;
      const down = padRamp(to, 0, 4);
      const rel = this.around(90, 0.35);
      for (let s = 0; s < down.length; s++) {
        await this.until(start, holdEnd + ((s + 1) * rel) / down.length);
        if (this.padGen !== gen) return;
        this.pad.b[i] = down[s]!;
        this.padSend();
      }
    } finally {
      this.padGive(refs);
      if (this.sendPad) this.padSend();
    }
  }

  /**
   * Play a short run of full pad states at game frame rate (`frameMs`, 60 a second by default), then
   * hold the last one for `holdMs` as a watchdog before letting go. A newer `padStream` (or `releaseAll`)
   * takes over at once, so a driver that predicts ahead can stream chunks that replace each other: the
   * pad never goes neutral between them. Each step names the buttons and axes it wants and everything
   * else is released, so nothing stays held from an earlier step. Unknown names throw before any send.
   */
  padStream(steps: { buttons?: Record<string, unknown>; axes?: Record<string, unknown> }[], frameMs = 1000 / 60, holdMs = 250): Promise<void> {
    if (!this.sendPad) throw new Error("no pad channel on this page");
    const parsed = steps.map((st) => {
      const b = new Array<number>(24).fill(0);
      const a = [0, 0, 0, 0];
      for (const [n, v] of Object.entries(st.buttons ?? {})) {
        const i = PAD_BUTTONS[n as keyof typeof PAD_BUTTONS];
        if (i === undefined) throw new Error(`unknown pad button: ${n} (use ${Object.keys(PAD_BUTTONS).join(", ")})`);
        b[i] = clampTo(v, 0, 1);
      }
      for (const [n, v] of Object.entries(st.axes ?? {})) {
        const i = PAD_AXES[n as keyof typeof PAD_AXES];
        if (i === undefined) throw new Error(`unknown pad axis: ${n} (use lx, ly, rx, ry)`);
        a[i] = clampTo(v, -1, 1);
      }
      return { b, a };
    });
    const gen = ++this.padGen;
    return (async () => {
      const t0 = performance.now();
      for (let i = 0; i < parsed.length; i++) {
        const wait = t0 + i * frameMs - performance.now();
        if (wait > 1) await sleep(wait);
        if (gen !== this.padGen) return;
        this.pad = parsed[i]!;
        this.padSend();
      }
      await sleep(Math.max(0, holdMs));
      if (gen === this.padGen) this.padNeutral();
    })();
  }

  /** Let go of the pad: every button and stick back to neutral, sent to the session. */
  padNeutral(): void {
    this.padGen++; // a run in flight stops where it is
    if (!this.sendPad) return;
    this.pad = { b: new Array<number>(24).fill(0), a: [0, 0, 0, 0] };
    this.padRefs.clear();
    this.padLast = "";
    this.padSend();
  }


  private canvas: OffscreenCanvas | null = null;
  private ctx: OffscreenCanvasRenderingContext2D | null = null;

  /**
   * The current picture, downscaled to `width` pixels wide (aspect kept). Reuses one canvas, so a
   * call costs a GPU downscale and a readback of `width`×`height`×4 bytes. Null before the first frame.
   */
  frame(width = 64): Frame | null {
    const v = this.video;
    if (v.readyState < 2 || !v.videoWidth) return null;
    const w = Math.max(1, Math.round(width));
    const h = Math.max(1, Math.round((w * v.videoHeight) / v.videoWidth));
    if (!this.canvas || this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas = new OffscreenCanvas(w, h);
      this.ctx = this.canvas.getContext("2d", { willReadFrequently: true });
    }
    if (!this.ctx) return null;
    this.ctx.drawImage(v, 0, 0, w, h);
    return { w, h, data: this.ctx.getImageData(0, 0, w, h).data };
  }

  /**
   * The picture (or a region of it) as a PNG, `width` pixels wide at most 1024 (aspect kept). Crops
   * from the video's own pixels, so a small region keeps its detail. Null before the first frame.
   */
  async png(width = 256, region?: Region): Promise<{ w: number; h: number; data: Uint8Array } | null> {
    const v = this.video;
    if (v.readyState < 2 || !v.videoWidth) return null;
    const r = region ?? WHOLE;
    const sx = Math.min(v.videoWidth - 1, Math.max(0, Math.round(r.x * v.videoWidth)));
    const sy = Math.min(v.videoHeight - 1, Math.max(0, Math.round(r.y * v.videoHeight)));
    const sw = Math.max(1, Math.min(v.videoWidth - sx, Math.round(r.w * v.videoWidth)));
    const sh = Math.max(1, Math.min(v.videoHeight - sy, Math.round(r.h * v.videoHeight)));
    const w = Math.min(1024, Math.max(1, Math.round(width)));
    const h = Math.max(1, Math.round((w * sh) / sw));
    const canvas = new OffscreenCanvas(w, h);
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    ctx.drawImage(v, sx, sy, sw, sh, 0, 0, w, h);
    const blob = await canvas.convertToBlob({ type: "image/png" });
    return { w, h, data: new Uint8Array(await blob.arrayBuffer()) };
  }

  /** The average colour of a region of a frame (the whole frame by default). */
  mean(f: Frame, r?: Region): { r: number; g: number; b: number } {
    return meanRgb(f, r);
  }

  /**
   * A colour grid over the current picture: `cols` cells across, each the mean of a
   * ~4px-wide patch. A few hundred bytes that stand in for the frame when a script
   * only needs to know where a bright or coloured thing is. Null before the first frame.
   */
  grid(cols = 8): Grid | null {
    const f = this.frame(Math.max(1, Math.round(cols)) * 4);
    return f && gridOf(f, cols);
  }

  /** How different two frames are, 0..255. */
  diff(a: Frame, b: Frame): number {
    return frameDiff(a, b);
  }

  /**
   * Resolves with the first frame, checked every `every` presented frames (default 3), that
   * satisfies `test`; null after `timeoutMs`. No timer polling: it runs on the video's own frames.
   */
  waitFor(test: (f: Frame) => boolean, timeoutMs = 5000, every = 3, width = 64): Promise<Frame | null> {
    return new Promise((resolve) => {
      const v = this.video;
      const deadline = performance.now() + timeoutMs;
      let n = 0;
      const onFrame = () => {
        if (performance.now() > deadline) return resolve(null);
        if (n++ % every === 0) {
          const f = this.frame(width);
          if (f && test(f)) return resolve(f);
        }
        v.requestVideoFrameCallback(onFrame);
      };
      v.requestVideoFrameCallback(onFrame);
      // A paused or stalled video presents no frames: the deadline still has to fire.
      setTimeout(() => resolve(null), timeoutMs + 250);
    });
  }

  /** Resolves when the picture differs from now by more than `threshold` (default 4), else null at the timeout. */
  async waitChange(timeoutMs = 5000, threshold = 4): Promise<Frame | null> {
    const base = this.frame();
    if (!base) return null;
    return this.waitFor((f) => frameDiff(base, f) > threshold, timeoutMs);
  }

  /** Restart the generator; the same seed replays the same timing. */
  seed(n: number): void {
    this.rand = rng(n);
  }

  /** A whole new run: let go of everything, reseed, re-centre the pointer. */
  reset(n = 1): void {
    this.releaseAll();
    this.rand = rng(n);
    this.rest.x = 0;
    this.rest.y = 0;
    const r = this.video.getBoundingClientRect();
    this.x = r.left + r.width / 2;
    this.y = r.top + r.height / 2;
  }

  /** Whether the picture has the mouse captured — only then does `look` turn a camera. */
  get locked(): boolean {
    return document.pointerLockElement === this.video;
  }

  /** A value around `ms`, log-normal (see `lognormal`): skewed late, hesitations included. */
  private around(ms: number, spread = 0.25): number {
    return lognormal(this.rand, ms, spread);
  }

  private focus(): void {
    this.video.focus({ preventScroll: true });
  }

  /** Sleep until `start` + `ms` on the absolute clock, so timer lag never accumulates. */
  private async until(start: number, ms: number): Promise<void> {
    const late = start + ms - performance.now();
    if (late > 0) await sleep(late);
  }

  /** Walk `items` across about `ms` on an absolute clock, one `each` per item. */
  private async paced<T>(items: T[], ms: number, each: (item: T) => void): Promise<void> {
    const start = performance.now();
    const n = Math.max(1, items.length);
    for (let i = 0; i < items.length; i++) {
      await this.until(start, ((i + 1) * ms) / n);
      each(items[i]!);
    }
  }

  wait(ms: number): Promise<void> {
    return sleep(this.around(ms));
  }

  /** A human reaction pause: ~280 ms, wider spread than a plain wait. */
  async react(ms = 280): Promise<void> {
    await sleep(this.around(ms, 0.35));
  }

  /** One key event, with the `key` value and modifier flags a real keyboard would send. */
  private key(type: "keydown" | "keyup", code: string, repeat = false): void {
    const mod = MOD_CODES[code as keyof typeof MOD_CODES];
    if (mod) this.mods[mod] = type === "keydown";
    if (type === "keydown") this.held.add(code);
    else this.held.delete(code);
    this.video.dispatchEvent(
      new KeyboardEvent(type, {
        code,
        key: keyFor(code, this.mods.shift),
        repeat,
        bubbles: true,
        cancelable: true,
        ctrlKey: this.mods.ctrl,
        altKey: this.mods.alt,
        shiftKey: this.mods.shift,
        metaKey: this.mods.meta,
      }),
    );
  }

  /**
   * Press, hold for about `ms`, release. Past ~300 ms the OS auto-repeat comes
   * with it (first repeat ~280 ms in, then ~33 ms apart) unless `repeat` is off.
   */
  async hold(code: string, ms: number, opts: { repeat?: boolean } = {}): Promise<void> {
    this.focus();
    this.key("keydown", code);
    try {
      const start = performance.now();
      const total = this.around(ms);
      if (opts.repeat !== false) {
        let at = this.around(280, 0.35);
        while (at < total - 25) {
          await this.until(start, at);
          this.key("keydown", code, true);
          at += this.around(33, 0.3);
        }
      }
      await this.until(start, total);
    } finally {
      this.key("keyup", code);
    }
  }

  /** A short press (about 80 ms). */
  tap(code: string): Promise<void> {
    return this.hold(code, 80);
  }

  /** Several keys held together for about `ms` (a diagonal run, sprint while moving, Ctrl+V). */
  async chord(codes: string[], ms: number): Promise<void> {
    this.focus();
    try {
      for (const c of codes) {
        this.key("keydown", c);
        await sleep(this.around(25, 0.5));
      }
      await this.until(performance.now(), this.around(ms));
      for (const c of codes.slice().reverse()) {
        this.key("keyup", c);
        await sleep(this.around(20, 0.5));
      }
    } finally {
      // A script that throws mid-chord must not leave keys stuck down remotely.
      this.releaseAll();
    }
  }

  /**
   * Type text like a hand would: digraph pace, word gaps, the rare hesitation,
   * Shift pressed only where the text needs it. Anything off the US layout throws.
   */
  async type(text: string): Promise<void> {
    this.focus();
    let prev = "";
    for (const ch of text) {
      const entry = codeForChar(ch);
      if (!entry) throw new Error(`type: no key for ${JSON.stringify(ch)}`);
      const [code, shift] = entry;
      if (shift !== this.mods.shift) this.key(shift ? "keydown" : "keyup", "ShiftLeft");
      const gap = ch === " " ? 110 : prev === " " ? 200 : prev === "" ? 60 : typeDelay(prev, ch);
      const lag = this.rand() < 0.04 ? 3 + this.rand() * 2 : 1; // a rare hesitation
      await sleep(this.around(gap) * lag);
      this.key("keydown", code);
      await sleep(this.around(65, 0.35));
      this.key("keyup", code);
      prev = ch;
    }
    if (this.mods.shift) this.key("keyup", "ShiftLeft");
  }

  /** The buttons currently held, as an event `buttons` bitmask. */
  private bits(): number {
    let b = 0;
    for (const x of this.clicking) b |= 1 << x;
    return b;
  }

  private pointer(dx: number, dy: number): void {
    this.x += dx;
    this.y += dy;
    // A real mouse reports whole counts; slow fractional steps would evaporate
    // if the far side rounds per event. Keep the true path, emit whole pixels,
    // carry the remainder — the sent deltas still sum to the move exactly.
    this.rest.x += dx;
    this.rest.y += dy;
    const mx = Math.trunc(this.rest.x);
    const my = Math.trunc(this.rest.y);
    this.rest.x -= mx;
    this.rest.y -= my;
    const type = "onpointerrawupdate" in this.video ? "pointerrawupdate" : "pointermove";
    this.video.dispatchEvent(
      new PointerEvent(type, {
        bubbles: true,
        pointerId: 1,
        pointerType: "mouse",
        isPrimary: true,
        buttons: this.bits(),
        clientX: this.x,
        clientY: this.y,
        movementX: mx,
        movementY: my,
      }),
    );
  }

  private pointerButton(type: "pointerdown" | "pointerup", button: number, detail: number): PointerEvent {
    return new PointerEvent(type, {
      bubbles: true,
      cancelable: true,
      pointerId: 1,
      pointerType: "mouse",
      isPrimary: true,
      button,
      buttons: this.bits(),
      detail,
      clientX: this.x,
      clientY: this.y,
    });
  }

  /** The compatibility mouse event browsers pair with every pointer button event. */
  private mouseButton(type: "mousedown" | "mouseup" | "dblclick", button: number, detail: number): MouseEvent {
    return new MouseEvent(type, {
      bubbles: true,
      cancelable: true,
      button,
      buttons: this.bits(),
      detail,
      clientX: this.x,
      clientY: this.y,
    });
  }

  /**
   * Move the pointer by (dx, dy) pixels over about `ms` (default: Fitts's law
   * for the distance). Fast moves flick — one ballistic leg that overshoots,
   * then a small correction; slow ones track a smooth minimum-jerk path.
   * A locked pointer turns the camera.
   */
  async look(dx: number, dy: number, ms?: number, style: "auto" | "flick" | "track" = "auto"): Promise<void> {
    const dist = Math.hypot(dx, dy);
    const total = this.around(ms ?? fittsMs(dist));
    const flick = style === "flick" || (style === "auto" && dist > 24 && dist / total > 1);
    if (!flick) {
      const bow = (this.rand() - 0.5) * 0.08;
      const path = pathSteps(dx, dy, Math.max(2, Math.round(total / 8)), bow);
      await this.paced(path, total, ([sx, sy]) => this.pointer(sx, sy));
      return;
    }
    const over = Math.min(0.12, (0.02 + 16 / dist) * (0.5 + this.rand()));
    const main = pathSteps(dx * (1 + over), dy * (1 + over), Math.max(3, Math.round((total * 0.75) / 8)), (this.rand() - 0.5) * 0.06);
    const fix = pathSteps(-dx * over, -dy * over, 2, 0);
    await this.paced(main, total * 0.75, ([sx, sy]) => this.pointer(sx, sy));
    await this.paced(fix, total * 0.25, ([sx, sy]) => this.pointer(sx, sy));
  }

  /**
   * Move to a spot in the picture (fractions, 0..1 of the letterboxed stream,
   * matching how `InputCapture` reads positions) over about `ms`.
   */
  async moveTo(fx: number, fy: number, ms?: number, style: "auto" | "flick" | "track" = "auto"): Promise<void> {
    const r = this.video.getBoundingClientRect();
    const pic = pictureSize(this.video);
    const vw = pic?.w || r.width;
    const vh = pic?.h || r.height;
    const scale = Math.min(r.width / vw, r.height / vh);
    const w = vw * scale;
    const h = vh * scale;
    const tx = r.left + (r.width - w) / 2 + fx * w;
    const ty = r.top + (r.height - h) / 2 + fy * h;
    await this.look(tx - this.x, ty - this.y, ms, style);
  }

  /** A click of `button` at the pointer: down, about 90 ms, up; `detail` 2 marks a double-click's second press. */
  async click(button = 0, detail = 1): Promise<void> {
    this.focus();
    try {
      this.clicking.add(button);
      this.video.dispatchEvent(this.pointerButton("pointerdown", button, detail));
      this.video.dispatchEvent(this.mouseButton("mousedown", button, detail));
      await sleep(this.around(90, 0.3));
    } finally {
      this.clicking.delete(button);
      this.video.dispatchEvent(this.pointerButton("pointerup", button, detail));
      this.video.dispatchEvent(this.mouseButton("mouseup", button, detail));
    }
  }

  /** A double-click: two presses ~120 ms apart, the second with detail 2, then the dblclick event. */
  async doubleClick(button = 0): Promise<void> {
    await this.click(button, 1);
    await sleep(this.around(120, 0.35)); // well inside the browser's 500 ms window
    await this.click(button, 2);
    this.video.dispatchEvent(this.mouseButton("dblclick", button, 2));
  }

  /** `n` clicks with a cadence that tires: ~6.5 clicks/s easing down as the hand tires. */
  async burst(n = 5, button = 0): Promise<void> {
    for (let i = 0; i < n; i++) {
      await this.click(button);
      if (i < n - 1) await sleep(this.around(1000 / (6.5 - i * 0.5), 0.3));
    }
  }

  /** Scroll by (dx, dy) pixels in human notches over about `ms`, at a ragged cadence. */
  async scroll(dy: number, dx = 0, ms = 350): Promise<void> {
    const n = Math.max(wheelNotches(dy).length, wheelNotches(dx).length);
    if (n === 0) return;
    const start = performance.now();
    const weights = Array.from({ length: n }, () => 0.4 + this.rand());
    const sum = weights.reduce((a, b) => a + b, 0);
    let at = 0;
    for (let i = 0; i < n; i++) {
      at += (weights[i]! / sum) * ms;
      await this.until(start, at);
      this.video.dispatchEvent(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          deltaMode: 0,
          deltaX: dx / n,
          deltaY: dy / n,
          clientX: this.x,
          clientY: this.y,
        }),
      );
    }
  }

  /**
   * Keep a colour target centred: each presented frame, find it, correct the camera by a
   * small `look`, until it is lost, the timeout passes, or `stop()` runs. The loop closes
   * in the page at frame rate, so a driver on a slow channel sets the policy and lets
   * this hold the aim. Camera corrections only move anything while the mouse is captured.
   */
  servo(policy: TrackPolicy, timeoutMs = 4000): ServoRun {
    const gain = policy.gain ?? 0.7;
    const deadzone = policy.deadzone ?? 0.01;
    const minPixels = policy.minPixels ?? 4;
    const width = policy.width ?? 96;
    const every = Math.max(1, policy.every ?? 3);
    const tickMs = policy.tickMs ?? 14;
    const where = policy.region ?? WHOLE;
    const v = this.video;
    const started = performance.now();
    let n = 0;
    let last: { x: number; y: number } | null = null;
    let manual = false;
    let finish: (r: ServoResult) => void = () => {};
    const result = new Promise<ServoResult>((resolve) => (finish = resolve));
    const end = (stopped: ServoResult["stopped"]): void => {
      finish({ stopped, frames: n, ms: performance.now() - started, error: last });
    };
    const tick = (): void => {
      if (manual) return end("manual");
      if (performance.now() - started > timeoutMs) return end("timeout");
      if (!this.locked) return end("unlocked");
      const f = this.frame(width);
      const c = f && centroidOf(f, where, policy.match);
      if (!c || c.n < minPixels) return end("lost");
      last = { x: c.x, y: c.y };
      n++;
      if (n % every === 0) {
        const ex = 0.5 - c.x;
        const ey = 0.5 - c.y;
        if (Math.hypot(ex, ey) > deadzone) {
          const r = v.getBoundingClientRect();
          void this.look(ex * gain * r.width, ey * gain * r.height, tickMs, "track").then(() =>
            v.requestVideoFrameCallback(tick),
          );
          return;
        }
      }
      v.requestVideoFrameCallback(tick);
    };
    v.requestVideoFrameCallback(tick);
    // A stalled picture presents no frames: the timeout still has to end the run.
    setTimeout(() => end("timeout"), timeoutMs + 250);
    return { result, stop: () => (manual = true) };
  }

  /** Let go of everything the harness holds: the pad, keys (newest first), then buttons, dropping the modifiers. */
  releaseAll(): void {
    this.padGen++; // a pad run in flight stops where it is, sending nothing further
    if (!this.padIdle()) this.padNeutral();
    for (const code of [...this.held].reverse()) this.key("keyup", code);
    for (const b of [...this.clicking].reverse()) {
      this.clicking.delete(b);
      this.video.dispatchEvent(this.pointerButton("pointerup", b, 1));
      this.video.dispatchEvent(this.mouseButton("mouseup", b, 1));
    }
  }
}

declare global {
  interface Window {
    __chaHarness?: Harness;
  }
}

/** Installs `window.__chaHarness` for this picture; call in dev builds only. */
export function installHarness(video: HTMLVideoElement, sendPad?: (msg: Record<string, unknown>) => void): void {
  window.__chaHarness = new Harness(video, sendPad);
}
