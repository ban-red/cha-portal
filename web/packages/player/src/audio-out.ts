// The WebTransport sound's output: decoded Opus goes to an AudioWorklet that plays it through a
// GainNode (volume, mute) to the speakers. It replaces a MediaStreamTrackGenerator feeding an
// `<audio>` element, a path Chrome twice stopped playing on (the stream flowed, the element said
// "playing", the tab had no speaker icon, and only quitting Chrome helped). A Web Audio graph
// we own can be told when it breaks and rebuilt.
//
// The worklet is audio-out.worklet.ts, bundled by Vite as its own file (`?worker&url`: in dev
// a module served by the dev server, in the build a hashed file under assets/) and loaded with
// `audioWorklet.addModule()`. Packets travel as transferred Float32Arrays: the portal isn't
// cross-origin isolated, so no SharedArrayBuffer.

import { SAMPLE_RATE } from "./audio-fifo";
import { contextAction, PeakWindow, StallDetector } from "./audio-watch";
import workletUrl from "./audio-out.worklet.ts?worker&url";
import type { SoundReport, ToSoundWorklet } from "./audio-out.worklet";

export interface AudioOutOptions {
  muted: boolean;
  volume: number;
  /** The context is waiting for a click or key press (autoplay), or no longer is. */
  onBlocked?: (blocked: boolean) => void;
  /** The output rebuilt itself (it broke), as opposed to `restart()`. */
  onRebuild?: (reason: string) => void;
}

export interface AudioOutStats {
  /** The largest sample actually played over the last second, 0..1; null while the context isn't running. */
  outPeak: number | null;
  /** The same for the sound pushed in, which is what the output should have played. */
  inPeak: number | null;
  /** Cumulative over rebuilds. */
  underruns: number;
  drops: number;
  /** Frames queued in the worklet now, as ms. */
  queuedMs: number;
}

/** How often the watchdog looks. */
const WATCH_MS = 1000;
/** Rebuilds no closer together than this. */
const MIN_REBUILD_GAP_MS = 2000;

export class AudioOut {
  /** Web Audio with worklets is there: the other path is the track and `<audio>` element. */
  static supported(): boolean {
    return typeof AudioContext !== "undefined" && typeof AudioWorkletNode !== "undefined";
  }

  private ctx: AudioContext | null = null;
  private node: AudioWorkletNode | null = null;
  private gain: GainNode | null = null;
  /** Bumped by every build and by close, so late callbacks of an old graph know to stand down. */
  private gen = 0;
  private muted: boolean;
  private volume: number;
  private blocked = false;
  /** The context has been running at least once (so a gesture was given). */
  private hasRun = false;
  private resumeTries = 0;
  /** The worklet module didn't load into this context. */
  private loadFailed = false;
  private lastBuild = 0;
  private closed = false;
  private readonly timer: ReturnType<typeof setInterval>;
  private readonly stall = new StallDetector();
  private readonly outPeak = new PeakWindow();
  private readonly inPeak = new PeakWindow();
  /** Frames pushed to the current worklet, and its latest report. */
  private pushed = 0;
  private report: SoundReport | null = null;
  /** Counters from worklets already replaced. */
  private baseUnderruns = 0;
  private baseDrops = 0;

  constructor(private readonly options: AudioOutOptions) {
    this.muted = options.muted;
    this.volume = options.volume;
    this.build();
    this.timer = setInterval(() => this.watch(), WATCH_MS);
    document.addEventListener("visibilitychange", this.onVisible);
  }

  /** The graph's context and the node that plays, for the latency probe to tap. Null until built. */
  tap(): { ctx: AudioContext; source: AudioNode } | null {
    return this.ctx && this.node ? { ctx: this.ctx, source: this.node } : null;
  }

  /** Queues one decoded packet (planar f32 whatever its format) and closes it. */
  push(data: AudioData): void {
    try {
      const node = this.node;
      const frames = data.numberOfFrames;
      if (!node || !this.ctx || frames === 0) return;
      const left = new Float32Array(frames);
      const right = data.numberOfChannels > 1 ? new Float32Array(frames) : left;
      data.copyTo(left, { planeIndex: 0, format: "f32-planar" });
      if (right !== left) data.copyTo(right, { planeIndex: 1, format: "f32-planar" });
      let peak = 0;
      for (let i = 0; i < frames; i++) peak = Math.max(peak, Math.abs(left[i]!), Math.abs(right[i]!));
      this.inPeak.note(performance.now(), peak);
      this.pushed += frames;
      const msg: ToSoundWorklet = { type: "push", left, right: right === left ? left.slice() : right };
      node.port.postMessage(msg, [msg.left.buffer, msg.right.buffer]);
    } catch {
      // A packet lost to a copy error; the stall watch notices if it keeps happening.
    } finally {
      data.close();
    }
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.applyGain();
    if (!muted) this.resume();
  }

  setVolume(volume: number): void {
    this.volume = volume;
    this.applyGain();
  }

  /** Starts a suspended context: from a click or key press this satisfies autoplay. */
  resume(): void {
    const ctx = this.ctx;
    if (!ctx || ctx.state === "running" || ctx.state === "closed") return;
    ctx.resume().then(
      () => {
        if (ctx === this.ctx) this.noteRunning();
      },
      () => {
        if (ctx === this.ctx) this.resumeTries++;
      },
    );
  }

  /** A new context and worklet (the page's "Restart sound"; not counted as a self-heal). */
  restart(): void {
    this.rebuild(null);
  }

  stats(): AudioOutStats {
    const now = performance.now();
    const running = this.ctx?.state === "running" && !this.blocked;
    return {
      // Running with no report in the last second is a worklet that isn't rendering: silent.
      outPeak: running ? (this.outPeak.peak(now) ?? 0) : null,
      inPeak: this.inPeak.peak(now),
      underruns: this.baseUnderruns + (this.report?.underruns ?? 0),
      drops: this.baseDrops + (this.report?.drops ?? 0),
      queuedMs: ((this.report?.queued ?? 0) / SAMPLE_RATE) * 1000,
    };
  }

  close(): void {
    this.closed = true;
    clearInterval(this.timer);
    document.removeEventListener("visibilitychange", this.onVisible);
    this.teardown();
  }

  private readonly onVisible = (): void => {
    if (document.visibilityState === "visible" && !this.muted) this.resume();
  };

  private applyGain(): void {
    const { gain, ctx } = this;
    if (!gain || !ctx) return;
    gain.gain.setTargetAtTime(this.muted ? 0 : this.volume, ctx.currentTime, 0.005);
  }

  private build(): void {
    const gen = ++this.gen;
    this.lastBuild = performance.now();
    this.pushed = 0;
    this.report = null;
    this.resumeTries = 0;
    this.loadFailed = false;
    this.stall.reset();
    let ctx: AudioContext;
    try {
      ctx = new AudioContext({ sampleRate: SAMPLE_RATE, latencyHint: "interactive" });
    } catch {
      return; // the watchdog tries again
    }
    this.ctx = ctx;
    ctx.onstatechange = () => {
      if (gen !== this.gen) return;
      if (ctx.state === "running") this.noteRunning();
      else this.setBlocked(!this.muted && !this.hasRun);
    };
    this.setBlocked(false);
    ctx.audioWorklet.addModule(workletUrl).then(
      () => {
        if (gen !== this.gen) return;
        const node = new AudioWorkletNode(ctx, "cha-sound", {
          numberOfInputs: 0,
          numberOfOutputs: 1,
          outputChannelCount: [2],
        });
        node.port.onmessage = (e: MessageEvent<SoundReport>) => {
          if (gen !== this.gen || e.data.type !== "report") return;
          this.report = e.data;
          this.outPeak.note(performance.now(), e.data.peak);
        };
        const gain = ctx.createGain();
        gain.gain.value = this.muted ? 0 : this.volume;
        node.connect(gain).connect(ctx.destination);
        this.node = node;
        this.gain = gain;
        if (ctx.state === "running") this.noteRunning();
        else {
          // Without a user gesture the context starts suspended and resume() stays pending until
          // one: that is the "Enable sound" prompt.
          this.setBlocked(!this.muted);
          this.resume();
        }
      },
      () => {
        // The module didn't load (a stale page after a deploy, say): the watchdog rebuilds.
        if (gen === this.gen) this.loadFailed = true;
      },
    );
  }

  private teardown(): void {
    this.gen++;
    const { ctx, node, gain } = this;
    this.baseUnderruns += this.report?.underruns ?? 0;
    this.baseDrops += this.report?.drops ?? 0;
    this.report = null;
    this.ctx = this.node = this.gain = null;
    if (node) {
      node.port.onmessage = null;
      node.disconnect();
    }
    gain?.disconnect();
    if (ctx) {
      ctx.onstatechange = null;
      void ctx.close().catch(() => undefined);
    }
  }

  private rebuild(reason: string | null): void {
    if (this.closed) return;
    this.teardown();
    this.build();
    if (reason !== null) this.options.onRebuild?.(reason);
  }

  private noteRunning(): void {
    this.hasRun = true;
    this.resumeTries = 0;
    this.setBlocked(false);
  }

  private setBlocked(blocked: boolean): void {
    if (blocked === this.blocked) return;
    this.blocked = blocked;
    this.options.onBlocked?.(blocked);
  }

  /** Once a second: resume a suspended context, rebuild a broken one, and catch an output that plays nothing. */
  private watch(): void {
    if (this.closed) return;
    const ctx = this.ctx;
    const now = performance.now();
    const since = now - this.lastBuild;
    if (!ctx) {
      if (since >= MIN_REBUILD_GAP_MS) this.rebuild("no audio context");
      return;
    }
    if (this.loadFailed && since >= MIN_REBUILD_GAP_MS) {
      this.rebuild("audio worklet didn't load");
      return;
    }
    const action = contextAction({
      state: ctx.state,
      wantSound: !this.muted,
      visible: document.visibilityState === "visible",
      hasRun: this.hasRun,
      resumeTries: this.resumeTries,
    });
    if (action === "rebuild" && since >= MIN_REBUILD_GAP_MS) {
      this.rebuild(ctx.state === "closed" ? "audio context closed" : "audio context won't resume");
      return;
    }
    if (action === "resume") {
      if (this.hasRun) this.resumeTries++;
      this.resume();
    }
    const stalled = this.stall.observe(now, this.pushed, this.report?.played ?? 0, ctx.state === "running" && this.node !== null);
    if (stalled && since >= MIN_REBUILD_GAP_MS) this.rebuild("audio output stalled");
  }
}
