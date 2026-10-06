// An adaptive playout buffer for the WebTransport audio path: Opus packets of 10 ms,
// numbered, stamped with the server's send time (µs, 32 bits). Pure: it is given packets
// and a clock and says what to play, so it tests without a browser.
//
// Datagrams arrive reordered, duplicated, late or not at all. The buffer holds a packet
// until its slot, `send time + base transit + delay`, and hands packets out in id order.
//   - Base transit is the smallest (arrival − send) of the last 10 s, so the two clocks'
//     offset cancels and what is left of a packet's transit is queueing.
//   - Delay is the p99 of that queueing over the last 5 s, clamped to [0, MAX_DELAY_MS].
//     It rises at once and falls at DECAY_MS_PER_S, so one burst isn't followed by a
//     second glitch when the buffer shrinks. On a quiet LAN the p99 is a fraction of a
//     millisecond and the delay stays 0: a packet plays the moment it arrives.
//   - A packet whose id was already handed out or skipped is dropped (late, or a
//     duplicate). A packet missing at its slot is concealed: WebCodecs' AudioDecoder
//     doesn't expose Opus PLC, so the caller plays 10 ms of silence for it instead of
//     stalling. A packet more than MAX_LATE_MS past its slot is dropped (to catch up,
//     not stack a backlog in the sink); a gap longer than that is skipped without
//     concealment (a stall, not a loss).

/** One packet's playout length. */
export const PACKET_MS = 10;
export const MAX_DELAY_MS = 80;
/** How far the delay may fall per second of stream. */
export const DECAY_MS_PER_S = 2;
/** Queueing delays kept for the p99. */
export const JITTER_WINDOW_MS = 5000;
/** Transit times kept for the base (their minimum). */
export const BASE_WINDOW_MS = 10_000;
/** Past its slot by this much, a packet is dropped rather than played. */
export const MAX_LATE_MS = 50;
/** Under this the target is 0: it is clock noise, not jitter. */
const DEADBAND_MS = 1.5;
/** An id this far behind the last one means the streamer restarted its count. */
const RESTART_GAP = 1000;
/** Ids skipped in one poll before it jumps to the next packet that is there. */
const MAX_SKIP = 100;
const MAX_QUEUE = 200;

export type AudioOut<T> =
  /** Decode and play this packet. */
  | { kind: "play"; id: number; data: T }
  /** The packet's slot came and it hasn't: play 10 ms of silence. */
  | { kind: "conceal"; id: number };

export interface AudioJitterStats {
  played: number;
  /** Slots filled with silence. */
  concealed: number;
  /** Arrived after their slot had passed, or twice. */
  late: number;
  /** Past MAX_LATE_MS behind, dropped to catch up. */
  dropped: number;
  /** Gaps skipped without concealment (a stall or a restart). */
  skipped: number;
}

interface Queued<T> {
  data: T;
  sendMs: number;
}

export class AudioJitter<T> {
  private readonly queue = new Map<number, Queued<T>>();
  /** The next id to hand out; null before the first packet. */
  private next: number | null = null;
  private prevSendTs: number | null = null;
  /** The send time (ms, unwrapped, the server's clock) of the latest packet by arrival, and its id. */
  private refSendMs = 0;
  private refId = 0;
  private transits: { at: number; ms: number }[] = [];
  private jitters: { at: number; ms: number }[] = [];
  private base = 0;
  private sinceTarget = 0;
  private target = 0;
  private delay = 0;
  private delayAt: number | null = null;
  readonly stats: AudioJitterStats = { played: 0, concealed: 0, late: 0, dropped: 0, skipped: 0 };

  /** The delay now, ms: what the buffer adds to a packet that arrives on time. */
  get delayMs(): number {
    return this.delay;
  }

  /**
   * A packet came, at `nowMs` on any clock that only moves forward in ms (not its own: only
   * differences are used). Follow with `poll`.
   */
  push(id: number, sendTsUs: number, nowMs: number, data: T): void {
    if (this.next !== null && id < this.next - RESTART_GAP) this.reset();
    // Unwrap the 32-bit µs stamp against the previous packet's, so a wrap or a reorder both come out right.
    let sendMs: number;
    if (this.prevSendTs === null) {
      sendMs = sendTsUs / 1000;
      this.refSendMs = sendMs;
    } else {
      const delta = ((sendTsUs - this.prevSendTs) | 0) / 1000;
      sendMs = this.refSendMs + delta;
    }
    this.prevSendTs = sendTsUs;
    this.refSendMs = sendMs;
    this.refId = id;

    this.measure(sendMs, nowMs);

    if (this.next === null) this.next = id;
    if (id < this.next || this.queue.has(id)) {
      this.stats.late++;
      return;
    }
    if (this.queue.size >= MAX_QUEUE) {
      // Only if the clock stops: don't grow without bound.
      const oldest = Math.min(...this.queue.keys());
      this.queue.delete(oldest);
      this.stats.dropped++;
    }
    this.queue.set(id, { data, sendMs });
  }

  /** What to play now, in order. Call after `push` and when `nextDueMs` has passed. */
  poll(nowMs: number): AudioOut<T>[] {
    const out: AudioOut<T>[] = [];
    this.adapt(nowMs);
    if (this.next === null) return out;
    let skipped = 0;
    for (;;) {
      const id: number = this.next;
      const p = this.queue.get(id);
      const due = this.dueMs(id, p);
      if (p) {
        if (nowMs < due) break;
        this.queue.delete(id);
        this.next = id + 1;
        if (nowMs - due > MAX_LATE_MS) this.stats.dropped++;
        else {
          this.stats.played++;
          out.push({ kind: "play", id, data: p.data });
        }
        continue;
      }
      if (this.queue.size === 0 || nowMs < due) break;
      const lateBy = nowMs - due;
      const ahead = Math.min(...this.queue.keys());
      if (ahead - id > MAX_SKIP) {
        // A long gap: the stream stalled or restarted its numbering. Resume at what is there.
        this.stats.skipped += ahead - id;
        this.next = ahead;
        continue;
      }
      this.next = id + 1;
      if (lateBy > MAX_LATE_MS || ++skipped > MAX_SKIP) this.stats.skipped++;
      else {
        this.stats.concealed++;
        out.push({ kind: "conceal", id });
      }
    }
    return out;
  }

  /** Milliseconds until the next packet or gap is due (0 if now), or null with nothing waiting. */
  nextDueMs(nowMs: number): number | null {
    if (this.next === null || this.queue.size === 0) return null;
    return Math.max(0, this.dueMs(this.next, this.queue.get(this.next)) - nowMs);
  }

  /** Slot of `id` on the local clock. A missing packet's send time is its neighbour's plus 10 ms a step. */
  private dueMs(id: number, p: Queued<T> | undefined): number {
    const sendMs = p ? p.sendMs : this.refSendMs + (id - this.refId) * PACKET_MS;
    return sendMs + this.base + this.delay;
  }

  private measure(sendMs: number, nowMs: number): void {
    const transit = nowMs - sendMs;
    this.transits.push({ at: nowMs, ms: transit });
    while (this.transits.length && nowMs - this.transits[0]!.at > BASE_WINDOW_MS) this.transits.shift();
    let min = Infinity;
    for (const t of this.transits) if (t.ms < min) min = t.ms;
    this.base = min;
    this.jitters.push({ at: nowMs, ms: Math.max(0, transit - min) });
    while (this.jitters.length && nowMs - this.jitters[0]!.at > JITTER_WINDOW_MS) this.jitters.shift();
    if (++this.sinceTarget >= 10) {
      this.sinceTarget = 0;
      const sorted = this.jitters.map((j) => j.ms).sort((a, b) => a - b);
      const p99 = sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.99))] ?? 0;
      this.target = p99 < DEADBAND_MS ? 0 : Math.min(MAX_DELAY_MS, p99);
    }
    this.adapt(nowMs);
  }

  private adapt(nowMs: number): void {
    const dt = this.delayAt === null ? 0 : Math.max(0, nowMs - this.delayAt);
    this.delayAt = nowMs;
    if (this.target >= this.delay) this.delay = this.target;
    else this.delay = Math.max(this.target, this.delay - (DECAY_MS_PER_S * dt) / 1000);
  }

  private reset(): void {
    this.queue.clear();
    this.next = null;
    this.prevSendTs = null;
    this.transits = [];
    this.jitters = [];
    this.sinceTarget = 0;
    this.target = 0;
    this.delay = 0;
    this.delayAt = null;
  }
}
