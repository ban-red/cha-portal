// Transport-independent run bookkeeping: control messages, clock sync pings,
// periodic progress, frame expiry and the final result.

import { ClockSync, FrameMeter, type MeterSnapshot, type MeterSummary } from "./meter";
import { LineDecoder, type SenderStats, type ServerMsg, type TrafficConfig, type TransportStats } from "./proto";

export type TransportKind = "webtransport" | "webrtc";

export interface RunProgress {
  snapshot: MeterSnapshot;
  server: (SenderStats & Partial<TransportStats>) | null;
  minRttMs: number | null;
}

export interface RunResult {
  transport: TransportKind;
  config: TrafficConfig;
  hello: Extract<ServerMsg, { t: "hello" }> | null;
  server: (SenderStats & Partial<TransportStats>) | null;
  minRttMs: number | null;
  summary: MeterSummary;
  endReason: string;
}

const PING_MS = 200;
const PROGRESS_MS = 250;
const EXPIRE_MS = 50;
/** After the server's "done", wait this long for stragglers. */
const TAIL_MS = 400;

export class Run {
  readonly clock = new ClockSync();
  readonly meter: FrameMeter;
  private readonly lines = new LineDecoder();
  private hello: Extract<ServerMsg, { t: "hello" }> | null = null;
  private server: (SenderStats & Partial<TransportStats>) | null = null;
  private timers: ReturnType<typeof setInterval>[] = [];
  private settled = false;
  private resolve!: (r: RunResult) => void;
  readonly result: Promise<RunResult>;

  constructor(
    private readonly transport: TransportKind,
    private readonly config: TrafficConfig,
    private readonly sendControl: (line: string) => void,
    private readonly onProgress: (p: RunProgress) => void,
  ) {
    this.meter = new FrameMeter(config.fps, () => this.hello?.fragments_per_frame ?? null);
    this.result = new Promise((resolve) => (this.resolve = resolve));
  }

  start(): void {
    const ping = () => this.sendControl(JSON.stringify({ t: "ping", c: performance.now() }) + "\n");
    // The first write also makes the control stream visible to the server.
    ping();
    this.timers.push(setInterval(ping, PING_MS));
    this.timers.push(setInterval(() => this.meter.expire(performance.now()), EXPIRE_MS));
    this.timers.push(
      setInterval(
        () =>
          this.onProgress({
            snapshot: this.meter.snapshot(performance.now()),
            server: this.server,
            minRttMs: this.clock.minRttMs,
          }),
        PROGRESS_MS,
      ),
    );
    // Safety net in case "done" never arrives.
    const deadline = (this.config.secs + 10) * 1000;
    this.timers.push(setTimeout(() => this.finish("timeout"), deadline));
  }

  onControl(chunk: Uint8Array | string): void {
    for (const msg of this.lines.push(chunk)) {
      switch (msg.t) {
        case "hello":
          this.hello = msg;
          break;
        case "pong":
          this.clock.onPong(msg.c, msg.s_us, performance.now());
          break;
        case "stats": {
          const { t: _t, elapsed_ms: _e, ...rest } = msg;
          this.server = rest;
          break;
        }
        case "done": {
          const { t: _t, ...rest } = msg;
          this.server = { ...this.server, ...rest };
          setTimeout(() => this.finish("done"), TAIL_MS);
          break;
        }
      }
    }
  }

  onDatagram(d: Uint8Array): void {
    this.meter.onDatagram(d, performance.now(), this.clock);
  }

  finish(reason: string): void {
    if (this.settled) return;
    this.settled = true;
    for (const t of this.timers) clearInterval(t);
    this.resolve({
      transport: this.transport,
      config: this.config,
      hello: this.hello,
      server: this.server,
      minRttMs: this.clock.minRttMs,
      summary: this.meter.finish(),
      endReason: reason,
    });
  }
}
