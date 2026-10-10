// Pad input → decoded frame, measured in the browser (dev builds only; reached as
// `window.__chaHarness.padProbe()`, see docs/benchmarks/ for the procedure).
//
// Meant for the test-pattern environment, whose centre square turns green while pad button A is
// held. Each sample presses A as the session's own pad (the same `pad` message real pads send,
// through the player's send path), then reads the decoded frames until the square is green. The
// time is from just before the message is handed to the player to the moment that frame arrives
// out of the decoder: not presented (a browser pane that never paints still decodes), so it leaves
// out the pad's USB hop and the display's scan-out.
//
// Frames come from a MediaStreamTrackProcessor on the video element's track: on WebTransport
// that is the track the player writes decoded VideoFrames into, on WebRTC the browser's own
// decoded track. The pixels are a 16x16 patch from the middle, copied out only while a sample waits.

import { percentile } from "./probe";

export interface PadProbeOptions {
  /** Samples to keep (default 100). */
  count?: number;
  /** A random extra wait between samples, ms (default [100, 400]). */
  gapMs?: [number, number];
  /** How long A is held once seen, ms (default 120). */
  holdMs?: number;
  /** Give up on a sample after this long, ms (default 1000). */
  timeoutMs?: number;
  /** Seed for the gaps (default 1). */
  seed?: number;
}

export interface Summary {
  p50: number | null;
  p95: number | null;
  p99: number | null;
  min: number | null;
  max: number | null;
  mean: number | null;
}

export interface PadProbeResult {
  /** What was measured, for the results file. */
  what: "pad A press sent → decoded frame shows it (not presented, not glass to glass)";
  userAgent: string;
  /** The decoded size: the track's, then the video element's. */
  decoded: { width: number; height: number; trackFrameRate: number | null };
  samples: number;
  missed: number;
  warmups: number;
  latencyMs: Summary;
  /** Every sample, in order. */
  all: number[];
  /** Decoded frame arrivals over the whole run. */
  frames: { count: number; fps: number; intervalMs: Summary };
  /** Arrival → patch read back, ms: done off the read loop, so it only matters if it passes a few frame intervals. */
  readMs: Summary;
  /** When the run began (ISO time). */
  startedAt: string;
  durationMs: number;
}

declare class MediaStreamTrackProcessor<T> {
  constructor(init: { track: MediaStreamTrack; maxBufferSize?: number });
  readonly readable: ReadableStream<T>;
}

export function summarize(values: number[]): Summary {
  if (!values.length) return { p50: null, p95: null, p99: null, min: null, max: null, mean: null };
  const r = (n: number | null) => (n === null ? null : Math.round(n * 100) / 100);
  return {
    p50: r(percentile(values, 0.5)),
    p95: r(percentile(values, 0.95)),
    p99: r(percentile(values, 0.99)),
    min: r(Math.min(...values)),
    max: r(Math.max(...values)),
    mean: r(values.reduce((a, b) => a + b, 0) / values.length),
  };
}

/** Whether a mean colour is the test pattern's pad-A green (0x00ff00, after a lossy codec). */
export function isPadGreen(r: number, g: number, b: number): boolean {
  return g > 150 && r < 120 && b < 120;
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** The mulberry32 generator the harness uses, kept local so this file stands alone. */
function rand(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export async function runPadProbe(
  video: HTMLVideoElement,
  /** Presses A and returns `performance.now()` taken just before the message is handed over. */
  press: () => number,
  release: () => void,
  options: PadProbeOptions = {},
): Promise<PadProbeResult> {
  const count = options.count ?? 100;
  const [gapLo, gapHi] = options.gapMs ?? [100, 400];
  const holdMs = options.holdMs ?? 120;
  const timeoutMs = options.timeoutMs ?? 1000;
  const next = rand(options.seed ?? 1);

  const stream = video.srcObject;
  const track = stream instanceof MediaStream ? stream.getVideoTracks()[0] : undefined;
  if (!track) throw new Error("no video track on the video element yet");
  if (typeof MediaStreamTrackProcessor === "undefined") throw new Error("this browser has no MediaStreamTrackProcessor");

  const arrivals: number[] = [];
  const readMs: number[] = [];
  let size = { width: 0, height: 0 };
  /** While set, every frame is read and the callback told whether the square is green. */
  let watch: ((on: boolean, at: number) => void) | null = null;
  let stopped = false;

  /**
   * Reads a 16x16 patch from the middle of the frame, off the read loop: the frame is stamped on
   * arrival and closed once the (asynchronous, a few ms) copy is back, so a slow read delays
   * nothing and the stamps stay the decoder's.
   */
  const classify = async (frame: VideoFrame, at: number, w: (on: boolean, at: number) => void) => {
    try {
      const v = frame.visibleRect ?? { x: 0, y: 0, width: frame.codedWidth, height: frame.codedHeight };
      const x = (Math.round(v.x + v.width * 0.5) - 8) & ~1;
      const y = (Math.round(v.y + v.height * 0.5) - 8) & ~1;
      const buf = new Uint8Array(16 * 16 * 4);
      await frame.copyTo(buf, { rect: { x, y, width: 16, height: 16 }, format: "RGBA" });
      readMs.push(performance.now() - at);
      let r = 0;
      let g = 0;
      let b = 0;
      for (let i = 0; i < buf.length; i += 4) {
        r += buf[i]!;
        g += buf[i + 1]!;
        b += buf[i + 2]!;
      }
      w(isPadGreen(r / 256, g / 256, b / 256), at);
    } catch {
      // a frame the copy failed on is just not a sample
    } finally {
      frame.close();
    }
  };

  const reader = new MediaStreamTrackProcessor<VideoFrame>({ track }).readable.getReader();
  const pump = (async () => {
    for (;;) {
      const { value: frame, done } = await reader.read();
      if (done || !frame) return;
      const at = performance.now();
      arrivals.push(at);
      if (!size.width) size = { width: frame.displayWidth, height: frame.displayHeight };
      const w = watch as ((on: boolean, at: number) => void) | null;
      if (w) void classify(frame, at, w);
      else frame.close();
      if (stopped) return;
    }
  })();

  /** Waits for the square to be `want`; resolves with the arrival time of the first frame that shows it, or null. */
  const until = (want: boolean, ms: number): Promise<number | null> =>
    new Promise((resolve) => {
      const timer = setTimeout(() => {
        watch = null;
        resolve(null);
      }, ms);
      watch = (on, at) => {
        if (on !== want) return;
        clearTimeout(timer);
        watch = null;
        resolve(at);
      };
    });

  const started = performance.now();
  const samples: number[] = [];
  let missed = 0;
  let warmups = 0;
  try {
    release(); // creates the pad (neutral) if the session has none yet
    await sleep(500);
    // Warm-up: the virtual pad appears in the environment, which notices it within about a second.
    for (let tries = 0; tries < 12; tries++) {
      watch = null;
      const seen = until(true, 1500);
      press();
      const at = await seen;
      release();
      if (at !== null) break;
      warmups++;
    }
    await until(false, 2000);
    await sleep(300);

    while (samples.length + missed < count) {
      await sleep(gapLo + next() * (gapHi - gapLo));
      const seen = until(true, timeoutMs);
      const t0 = press();
      const at = await seen;
      if (at === null) missed++;
      else samples.push(at - t0);
      await sleep(holdMs);
      release();
      // Wait for the square to go, so the next sample starts from dark.
      if ((await until(false, 1500)) === null) await sleep(500);
    }
  } finally {
    watch = null;
    stopped = true;
    release();
    void reader.cancel().catch(() => {});
    await pump.catch(() => {});
  }
  const end = performance.now();
  const run = arrivals.filter((t) => t >= started);
  const gaps = run.slice(1).map((t, i) => t - run[i]!);
  return {
    what: "pad A press sent → decoded frame shows it (not presented, not glass to glass)",
    userAgent: navigator.userAgent,
    decoded: { ...size, trackFrameRate: track.getSettings().frameRate ?? null },
    samples: samples.length,
    missed,
    warmups,
    latencyMs: summarize(samples),
    all: samples.map((v) => Math.round(v * 100) / 100),
    frames: {
      count: run.length,
      fps: run.length > 1 ? Math.round((((run.length - 1) * 1000) / (run.at(-1)! - run[0]!)) * 10) / 10 : 0,
      intervalMs: summarize(gaps),
    },
    readMs: summarize(readMs),
    startedAt: new Date(Date.now() - (end - started)).toISOString(),
    durationMs: Math.round(end - started),
  };
}
