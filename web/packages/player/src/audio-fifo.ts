// The FIFO between the audio packets and the sound card's render quanta, for the Web Audio output
// (audio-out.ts). Pure: it is given planar frames and asked for the next quantum, so it tests
// without a browser; the worklet (audio-out.worklet.ts) runs this same class.
//
// The jitter buffer (audio-jitter.ts) already paces packets to their slots, so this only absorbs
// the device's timing: packets come every 10 ms (480 frames) in whole steps, the device asks for
// 128 frames every 2.67 ms.
//   - Output starts once `prefillFrames` are queued, and again after every underrun. A queue that
//     starts at one packet runs dry before the next arrives (480 is not a multiple of 128), so the
//     default is one packet plus one quantum.
//   - An empty queue outputs silence (the rest of the quantum after a partial one) and counts an
//     underrun.
//   - A queue past `capFrames` (a stalled device that came back, a burst after a busy main
//     thread) drops its oldest frames down to `catchUpFrames` and counts the drop: catching up
//     beats sound that runs seconds late.

export const SAMPLE_RATE = 48_000;
/** What a render quantum is in every browser's Web Audio today. */
export const QUANTUM_FRAMES = 128;
/** One 10 ms packet. */
export const PACKET_FRAMES = 480;
/** Frames queued before output starts: a packet plus a quantum, about 12.7 ms. */
export const DEFAULT_PREFILL_FRAMES = PACKET_FRAMES + QUANTUM_FRAMES;
/** Beyond the prefill, 60 ms of queue is tolerated before dropping. */
export const DEFAULT_CAP_EXTRA_FRAMES = 2880;
/** The ring's size in frames: well over the cap. */
const RING_FRAMES = 8192;

export interface FifoOptions {
  prefillFrames?: number;
  /** Queue length that triggers a drop. */
  capFrames?: number;
  /** Queue length a drop trims to. */
  catchUpFrames?: number;
}

export interface FifoCounters {
  /** Real (not silent) frames handed out. */
  played: number;
  /** Frames accepted. */
  pushed: number;
  underruns: number;
  /** Drop events, and the frames they dropped. */
  drops: number;
  droppedFrames: number;
  /** Frames queued now. */
  queued: number;
}

export class SoundFifo {
  private readonly left = new Float32Array(RING_FRAMES);
  private readonly right = new Float32Array(RING_FRAMES);
  private head = 0;
  private size = 0;
  private playing = false;
  private readonly prefill: number;
  private readonly cap: number;
  private readonly catchUp: number;
  private playedFrames = 0;
  private pushedFrames = 0;
  private underrunCount = 0;
  private dropCount = 0;
  private droppedFrameCount = 0;
  /** The largest absolute sample handed out since `takePeak()`. */
  private peak = 0;

  constructor(options: FifoOptions = {}) {
    this.prefill = options.prefillFrames ?? DEFAULT_PREFILL_FRAMES;
    this.cap = Math.min(options.capFrames ?? this.prefill + DEFAULT_CAP_EXTRA_FRAMES, RING_FRAMES - PACKET_FRAMES);
    this.catchUp = Math.min(options.catchUpFrames ?? this.prefill + PACKET_FRAMES, this.cap);
  }

  /** Queues planar frames. */
  push(left: Float32Array, right: Float32Array): void {
    let n = Math.min(left.length, right.length);
    // More than the ring holds in one go (not a thing at 10 ms packets): keep the newest.
    const skip = Math.max(0, n - RING_FRAMES);
    if (skip > 0) n -= skip;
    this.pushedFrames += n;
    if (this.size + n > this.cap) this.dropTo(Math.max(0, this.catchUp - n));
    let w = (this.head + this.size) % RING_FRAMES;
    for (let i = 0; i < n; i++) {
      this.left[w] = left[skip + i]!;
      this.right[w] = right[skip + i]!;
      w = w + 1 === RING_FRAMES ? 0 : w + 1;
    }
    this.size += n;
    if (!this.playing && this.size >= this.prefill) this.playing = true;
  }

  /** Fills the output quantum, silence where the queue has nothing. Returns the real frames written. */
  pull(outLeft: Float32Array, outRight: Float32Array | undefined): number {
    const want = outLeft.length;
    let n = 0;
    if (this.playing) {
      n = Math.min(want, this.size);
      let r = this.head;
      let peak = this.peak;
      for (let i = 0; i < n; i++) {
        const l = this.left[r]!;
        const rt = this.right[r]!;
        outLeft[i] = l;
        if (outRight) outRight[i] = rt;
        const a = Math.max(Math.abs(l), Math.abs(rt));
        if (a > peak) peak = a;
        r = r + 1 === RING_FRAMES ? 0 : r + 1;
      }
      this.peak = peak;
      this.head = r;
      this.size -= n;
      this.playedFrames += n;
      if (n < want) {
        this.underrunCount++;
        this.playing = false;
      }
    }
    for (let i = n; i < want; i++) {
      outLeft[i] = 0;
      if (outRight) outRight[i] = 0;
    }
    return n;
  }

  /** The largest sample handed out since the last call, 0..1+; resets it. */
  takePeak(): number {
    const p = this.peak;
    this.peak = 0;
    return p;
  }

  counters(): FifoCounters {
    return {
      played: this.playedFrames,
      pushed: this.pushedFrames,
      underruns: this.underrunCount,
      drops: this.dropCount,
      droppedFrames: this.droppedFrameCount,
      queued: this.size,
    };
  }

  /** Drops the oldest frames until `keep` are left. */
  private dropTo(keep: number): void {
    const drop = this.size - keep;
    if (drop <= 0) return;
    this.head = (this.head + drop) % RING_FRAMES;
    this.size = keep;
    this.dropCount++;
    this.droppedFrameCount += drop;
  }
}
