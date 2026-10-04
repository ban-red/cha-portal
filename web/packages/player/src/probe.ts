// Click → screen (and → speaker), measured in the browser (the S2/S4 probe):
// synthetic clicks go to the environment, and the first presented frame whose
// centre turns bright closes each one. Meant for the test-pattern environment,
// which flashes a white square in the middle on every click and plays a short
// tone with it. The tone's onset, timed by an AudioWorklet and mapped to when
// it leaves the speakers, gives click → sound and the A/V offset.

const FLASH_LUMA = 200;
const ONSET_LEVEL = 0.1;

export interface ProbeResult {
  samples: number;
  missed: number;
  /** Click sent → flash presented by this browser. */
  clickToPresentedMs: { p50: number | null; p95: number | null };
  /** Click sent → the streamer received it. */
  clickToStreamerMs: { p50: number | null; p95: number | null };
  /** Clicks whose tone was heard (0 without an audio track). */
  audioSamples: number;
  /** Click sent → the tone at the speakers. */
  clickToAudioMs: { p50: number | null; p95: number | null };
  /** Tone − flash for the same click: positive when sound comes later. */
  avOffsetMs: { p50: number | null; p95: number | null };
}

interface Sample {
  id: number;
  clickAt: number;
  ackAt?: number;
  presentedAt?: number;
  heardAt?: number;
}

export function percentile(values: number[], q: number): number | null {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))]!;
}

// Posts the context time of the first sample above the level after each "arm".
const ONSET_WORKLET = `
class Onset extends AudioWorkletProcessor {
  constructor() {
    super();
    this.armed = false;
    this.port.onmessage = () => { this.armed = true; };
  }
  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (this.armed && ch) {
      for (let i = 0; i < ch.length; i++) {
        if (Math.abs(ch[i]) > ${ONSET_LEVEL}) {
          this.armed = false;
          this.port.postMessage(currentTime + i / sampleRate);
          break;
        }
      }
    }
    return true;
  }
}
registerProcessor("cha-onset", Onset);
`;

export class ClickProbe {
  private readonly samples: Sample[] = [];
  private pending: Sample | null = null;
  private armed: Sample | null = null;
  private readonly ctx2d: CanvasRenderingContext2D;
  private timer?: ReturnType<typeof setInterval>;
  private audio: { ctx: AudioContext; node: AudioWorkletNode } | null = null;

  constructor(
    private readonly video: HTMLVideoElement,
    private readonly send: (msg: Record<string, unknown>) => void,
    private readonly toLocal: (serverUs: number) => number | null,
    private readonly audioTrack: MediaStreamTrack | null = null,
  ) {
    const c = document.createElement("canvas");
    c.width = c.height = 4;
    this.ctx2d = c.getContext("2d", { willReadFrequently: true })!;
  }

  /** Clicks every `intervalMs`, `count` times; resolves with the summary. */
  async run(count: number, intervalMs = 500): Promise<ProbeResult> {
    await this.listen();
    return new Promise((resolve) => {
      this.timer = setInterval(() => {
        if (this.samples.length >= count) {
          clearInterval(this.timer);
          setTimeout(() => {
            this.stop();
            resolve(this.summary());
          }, intervalMs);
          return;
        }
        this.pending = { id: this.samples.length, clickAt: performance.timeOrigin + performance.now() };
        this.samples.push(this.pending);
        if (this.audio) {
          this.armed = this.pending;
          this.audio.node.port.postMessage("arm");
        }
        this.send({ k: "move", x: 0.75, y: 0.25 });
        this.send({ k: "button", b: 0, down: true, probe: this.pending.id });
        this.send({ k: "button", b: 0, down: false });
      }, intervalMs);
    });
  }

  stop(): void {
    clearInterval(this.timer);
    void this.audio?.ctx.close().catch(() => {});
    this.audio = null;
  }

  /** The streamer received click `id` at `serverUs` (its clock). */
  acknowledged(id: number, serverUs: number): void {
    const s = this.samples[id];
    const at = this.toLocal(serverUs);
    if (s && at !== null) s.ackAt = at;
  }

  /** Called for every presented frame. */
  onFrame(md: VideoFrameCallbackMetadata): void {
    const s = this.pending;
    const w = this.video.videoWidth;
    const h = this.video.videoHeight;
    if (!s || !w) return;
    this.ctx2d.drawImage(this.video, w * 0.48, h * 0.48, w * 0.04, h * 0.04, 0, 0, 4, 4);
    const d = this.ctx2d.getImageData(0, 0, 4, 4).data;
    let luma = 0;
    for (let i = 0; i < d.length; i += 4) luma += 0.2126 * d[i]! + 0.7152 * d[i + 1]! + 0.0722 * d[i + 2]!;
    if (luma / 16 < FLASH_LUMA) return;
    s.presentedAt = performance.timeOrigin + md.presentationTime;
    this.pending = null;
  }

  /** Taps the audio track for tone onsets; without one, video only. */
  private async listen(): Promise<void> {
    if (!this.audioTrack || this.audioTrack.readyState !== "live") return;
    let ctx: AudioContext | null = null;
    try {
      ctx = new AudioContext({ latencyHint: "interactive" });
      const url = URL.createObjectURL(new Blob([ONSET_WORKLET], { type: "text/javascript" }));
      await ctx.audioWorklet.addModule(url);
      URL.revokeObjectURL(url);
      const node = new AudioWorkletNode(ctx, "cha-onset");
      ctx.createMediaStreamSource(new MediaStream([this.audioTrack])).connect(node);
      // Pulled by the destination; it outputs silence.
      node.connect(ctx.destination);
      node.port.onmessage = (e: MessageEvent<number>) => this.heard(e.data);
      await ctx.resume();
      this.audio = { ctx, node };
    } catch {
      void ctx?.close().catch(() => {});
    }
  }

  /** A tone began at `contextTime`: when that reaches the speakers. */
  private heard(contextTime: number): void {
    const s = this.armed;
    if (!s || !this.audio) return;
    const ts = this.audio.ctx.getOutputTimestamp();
    if (ts.contextTime === undefined || ts.performanceTime === undefined) return;
    s.heardAt = performance.timeOrigin + ts.performanceTime + (contextTime - ts.contextTime) * 1000;
    this.armed = null;
  }

  private summary(): ProbeResult {
    const done = this.samples.filter((s) => s.presentedAt !== undefined);
    const total = done.map((s) => s.presentedAt! - s.clickAt);
    const ack = this.samples.flatMap((s) => (s.ackAt === undefined ? [] : [s.ackAt - s.clickAt]));
    const heard = this.samples.filter((s) => s.heardAt !== undefined);
    const toAudio = heard.map((s) => s.heardAt! - s.clickAt);
    const offset = heard.flatMap((s) => (s.presentedAt === undefined ? [] : [s.heardAt! - s.presentedAt]));
    const p = (v: number[]) => ({ p50: percentile(v, 0.5), p95: percentile(v, 0.95) });
    return {
      samples: done.length,
      missed: this.samples.length - done.length,
      clickToPresentedMs: p(total),
      clickToStreamerMs: p(ack),
      audioSamples: heard.length,
      clickToAudioMs: p(toAudio),
      avOffsetMs: p(offset),
    };
  }
}
