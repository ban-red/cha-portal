// Click → screen, measured in the browser (the S2/S4 probe): synthetic clicks
// go to the environment, and the first presented frame whose centre turns
// bright closes each one. Meant for the test-pattern environment, which
// flashes a white square in the middle on every click.

const FLASH_LUMA = 200;

export interface ProbeResult {
  samples: number;
  missed: number;
  /** Click sent → flash presented by this browser. */
  clickToPresentedMs: { p50: number | null; p95: number | null };
  /** Click sent → the streamer received it. */
  clickToStreamerMs: { p50: number | null; p95: number | null };
}

interface Sample {
  id: number;
  clickAt: number;
  ackAt?: number;
  presentedAt?: number;
}

export function percentile(values: number[], q: number): number | null {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))]!;
}

export class ClickProbe {
  private readonly samples: Sample[] = [];
  private pending: Sample | null = null;
  private readonly ctx: CanvasRenderingContext2D;
  private timer?: ReturnType<typeof setInterval>;

  constructor(
    private readonly video: HTMLVideoElement,
    private readonly send: (msg: Record<string, unknown>) => void,
    private readonly toLocal: (serverUs: number) => number | null,
  ) {
    const c = document.createElement("canvas");
    c.width = c.height = 4;
    this.ctx = c.getContext("2d", { willReadFrequently: true })!;
  }

  /** Clicks every `intervalMs`, `count` times; resolves with the summary. */
  run(count: number, intervalMs = 500): Promise<ProbeResult> {
    return new Promise((resolve) => {
      this.timer = setInterval(() => {
        if (this.samples.length >= count) {
          clearInterval(this.timer);
          setTimeout(() => resolve(this.summary()), intervalMs);
          return;
        }
        this.pending = { id: this.samples.length, clickAt: performance.timeOrigin + performance.now() };
        this.samples.push(this.pending);
        this.send({ k: "move", x: 0.75, y: 0.25 });
        this.send({ k: "button", b: 0, down: true, probe: this.pending.id });
        this.send({ k: "button", b: 0, down: false });
      }, intervalMs);
    });
  }

  stop(): void {
    clearInterval(this.timer);
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
    this.ctx.drawImage(this.video, w * 0.48, h * 0.48, w * 0.04, h * 0.04, 0, 0, 4, 4);
    const d = this.ctx.getImageData(0, 0, 4, 4).data;
    let luma = 0;
    for (let i = 0; i < d.length; i += 4) luma += 0.2126 * d[i]! + 0.7152 * d[i + 1]! + 0.0722 * d[i + 2]!;
    if (luma / 16 < FLASH_LUMA) return;
    s.presentedAt = performance.timeOrigin + md.presentationTime;
    this.pending = null;
  }

  private summary(): ProbeResult {
    const done = this.samples.filter((s) => s.presentedAt !== undefined);
    const total = done.map((s) => s.presentedAt! - s.clickAt);
    const ack = this.samples.flatMap((s) => (s.ackAt === undefined ? [] : [s.ackAt - s.clickAt]));
    return {
      samples: done.length,
      missed: this.samples.length - done.length,
      clickToPresentedMs: { p50: percentile(total, 0.5), p95: percentile(total, 0.95) },
      clickToStreamerMs: { p50: percentile(ack, 0.5), p95: percentile(ack, 0.95) },
    };
  }
}
