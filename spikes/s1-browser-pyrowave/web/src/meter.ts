// Receive-side measurement for S1: per-frame reassembly bookkeeping, loss,
// throughput, frame completion spread and clock-synced one-way latency.

import { decodeHeader } from "./proto";

/** NTP-style offset estimate from ping/pong pairs; keeps the lowest-RTT sample. */
export class ClockSync {
  private best: { rttMs: number; offsetMs: number } | null = null;

  onPong(clientSentMs: number, serverUs: number, nowMs: number): void {
    const rttMs = nowMs - clientSentMs;
    const offsetMs = serverUs / 1000 - (clientSentMs + nowMs) / 2;
    if (!this.best || rttMs < this.best.rttMs) this.best = { rttMs, offsetMs };
  }

  get synced(): boolean {
    return this.best !== null;
  }

  get minRttMs(): number | null {
    return this.best?.rttMs ?? null;
  }

  /** Converts a sender session timestamp to this clock (ms). */
  toLocalMs(sendTsUs: number): number | null {
    return this.best ? sendTsUs / 1000 - this.best.offsetMs : null;
  }
}

interface FrameState {
  total: number;
  received: number;
  seen: Uint8Array;
  firstMs: number;
  lastMs: number;
  sendTsUs: number;
}

export interface Percentiles {
  p50: number | null;
  p95: number | null;
  p99: number | null;
  max: number | null;
}

export interface MeterSnapshot {
  elapsedMs: number;
  rateMbps: number;
  datagramsPerSec: number;
  datagrams: number;
  framesComplete: number;
  framesIncomplete: number;
  framesInFlight: number;
}

export interface MeterSummary {
  durationMs: number;
  datagrams: number;
  bytes: number;
  duplicates: number;
  malformed: number;
  lateDatagrams: number;
  framesComplete: number;
  framesIncomplete: number;
  framesMissing: number;
  /** Fraction of expected datagrams that never arrived (0..1). */
  datagramLoss: number;
  rateMbps: { median: number | null; min: number | null; mean: number | null };
  /** Last fragment arrival minus first fragment arrival, per complete frame. */
  spreadMs: Percentiles;
  /** First fragment arrival minus send time (clock-synced). */
  latencyFirstMs: Percentiles;
  /** Last fragment arrival minus send time (clock-synced). */
  latencyCompleteMs: Percentiles;
  /** |actual - nominal| interval between consecutive frame completions. */
  completionJitterMs: Percentiles;
}

const FRAME_TIMEOUT_MS = 250;
const RATE_WINDOW_MS = 1000;

export class FrameMeter {
  private readonly frames = new Map<number, FrameState>();
  private readonly finished = new Set<number>();
  private readonly spread: number[] = [];
  private readonly latFirst: number[] = [];
  private readonly latComplete: number[] = [];
  private readonly jitter: number[] = [];
  private readonly rates: number[] = [];

  private startMs: number | null = null;
  private lastMs = 0;
  private datagrams = 0;
  private bytes = 0;
  private duplicates = 0;
  private malformed = 0;
  private late = 0;
  private framesComplete = 0;
  private framesIncomplete = 0;
  private fragsExpected = 0;
  private fragsReceived = 0;
  private maxFrameId = -1;
  private lastCompletionMs: number | null = null;

  private windowStartMs = 0;
  private windowBytes = 0;
  private windowDatagrams = 0;
  private lastRateMbps = 0;
  private lastDps = 0;

  constructor(
    private readonly fps: number,
    private readonly fragmentsPerFrame: () => number | null,
  ) {}

  onDatagram(d: Uint8Array, nowMs: number, clock: ClockSync): void {
    const h = decodeHeader(d);
    if (!h) {
      this.malformed++;
      return;
    }
    if (this.startMs === null) {
      this.startMs = nowMs;
      this.windowStartMs = nowMs;
    }
    this.lastMs = nowMs;
    this.datagrams++;
    this.bytes += d.byteLength;
    this.windowBytes += d.byteLength;
    this.windowDatagrams++;
    this.rollWindow(nowMs);

    if (h.frameId > this.maxFrameId) this.maxFrameId = h.frameId;
    if (this.finished.has(h.frameId)) {
      this.late++;
      return;
    }
    let frame = this.frames.get(h.frameId);
    if (!frame) {
      frame = {
        total: h.fragCount,
        received: 0,
        seen: new Uint8Array(h.fragCount),
        firstMs: nowMs,
        lastMs: nowMs,
        sendTsUs: h.sendTsUs,
      };
      this.frames.set(h.frameId, frame);
    }
    if (h.fragIndex >= frame.total || frame.seen[h.fragIndex]) {
      this.duplicates++;
      return;
    }
    frame.seen[h.fragIndex] = 1;
    frame.received++;
    frame.lastMs = nowMs;
    if (frame.received === frame.total) this.complete(h.frameId, frame, clock);
  }

  private complete(frameId: number, f: FrameState, clock: ClockSync): void {
    this.frames.delete(frameId);
    this.markFinished(frameId);
    this.framesComplete++;
    this.fragsExpected += f.total;
    this.fragsReceived += f.received;
    this.spread.push(f.lastMs - f.firstMs);
    const sentLocal = clock.toLocalMs(f.sendTsUs);
    if (sentLocal !== null) {
      this.latFirst.push(f.firstMs - sentLocal);
      this.latComplete.push(f.lastMs - sentLocal);
    }
    if (this.lastCompletionMs !== null) {
      this.jitter.push(Math.abs(f.lastMs - this.lastCompletionMs - 1000 / this.fps));
    }
    this.lastCompletionMs = f.lastMs;
  }

  /** Gives up on frames that have been incomplete for too long. */
  expire(nowMs: number, maxAgeMs = FRAME_TIMEOUT_MS): void {
    for (const [id, f] of this.frames) {
      if (nowMs - f.firstMs < maxAgeMs) continue;
      this.frames.delete(id);
      this.markFinished(id);
      this.framesIncomplete++;
      this.fragsExpected += f.total;
      this.fragsReceived += f.received;
    }
  }

  private markFinished(id: number): void {
    this.finished.add(id);
    if (this.finished.size > 1024) {
      for (const old of this.finished) {
        if (old < this.maxFrameId - 512) this.finished.delete(old);
      }
    }
  }

  private rollWindow(nowMs: number): void {
    const span = nowMs - this.windowStartMs;
    if (span < RATE_WINDOW_MS) return;
    this.lastRateMbps = (this.windowBytes * 8) / span / 1000;
    this.lastDps = (this.windowDatagrams * 1000) / span;
    this.rates.push(this.lastRateMbps);
    this.windowStartMs = nowMs;
    this.windowBytes = 0;
    this.windowDatagrams = 0;
  }

  snapshot(nowMs: number): MeterSnapshot {
    return {
      elapsedMs: this.startMs === null ? 0 : nowMs - this.startMs,
      rateMbps: this.lastRateMbps,
      datagramsPerSec: this.lastDps,
      datagrams: this.datagrams,
      framesComplete: this.framesComplete,
      framesIncomplete: this.framesIncomplete,
      framesInFlight: this.frames.size,
    };
  }

  /** Expires everything still in flight and returns the run summary. */
  finish(): MeterSummary {
    this.expire(Number.POSITIVE_INFINITY, 0);
    const seenFrames = this.maxFrameId + 1;
    const framesMissing = Math.max(0, seenFrames - this.framesComplete - this.framesIncomplete);
    const perFrame = this.fragmentsPerFrame() ?? 0;
    const expected = this.fragsExpected + framesMissing * perFrame;
    const received = this.fragsReceived;
    // The first and last windows are partial (ramp-up / tail); drop them when we can.
    const steady = this.rates.length > 2 ? this.rates.slice(1) : this.rates;
    return {
      durationMs: this.startMs === null ? 0 : this.lastMs - this.startMs,
      datagrams: this.datagrams,
      bytes: this.bytes,
      duplicates: this.duplicates,
      malformed: this.malformed,
      lateDatagrams: this.late,
      framesComplete: this.framesComplete,
      framesIncomplete: this.framesIncomplete,
      framesMissing,
      datagramLoss: expected > 0 ? 1 - received / expected : 0,
      rateMbps: {
        median: percentile(steady, 0.5),
        min: steady.length ? steady.reduce((a, b) => Math.min(a, b)) : null,
        mean: steady.length ? steady.reduce((a, b) => a + b, 0) / steady.length : null,
      },
      spreadMs: percentiles(this.spread),
      latencyFirstMs: percentiles(this.latFirst),
      latencyCompleteMs: percentiles(this.latComplete),
      completionJitterMs: percentiles(this.jitter),
    };
  }
}

function pick(sorted: number[], q: number): number | null {
  if (sorted.length === 0) return null;
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil(q * sorted.length) - 1));
  return sorted[idx] ?? null;
}

function percentile(values: number[], q: number): number | null {
  return pick([...values].sort((a, b) => a - b), q);
}

function percentiles(values: number[]): Percentiles {
  const sorted = [...values].sort((a, b) => a - b);
  return {
    p50: pick(sorted, 0.5),
    p95: pick(sorted, 0.95),
    p99: pick(sorted, 0.99),
    max: sorted.at(-1) ?? null,
  };
}
