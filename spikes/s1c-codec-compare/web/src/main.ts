import { PyroWaveDecoder, PyroWaveDevice, YuvRenderer } from "@cha/pyrowave-webgpu";
import { VideoFrameRenderer } from "./video-renderer";
import type { StartMsg, WorkerMsg } from "./worker";

interface StreamInfo {
  name: string;
  codec: "pyrowave" | "hevc" | "h264" | "av1";
  codecString: string | null;
  width: number;
  height: number;
  chroma: "420" | "444";
  fps: number;
  bitrateMbps: number;
  frames: number;
}

interface ServerInfo {
  wt_port: number;
  cert_hash_hex: string;
  /** The S3 gateway: live frames from a GameStream host, WebRTC only. */
  gateway?: boolean;
  /** The S2 streamer: forwards keyboard and mouse from the video to the compositor. */
  input?: boolean;
  /** The S2 streamer can resize its output while streaming. */
  resize?: boolean;
}

/**
 * How a frame reaches the screen:
 * - webgpu: WebTransport → WebCodecs (or PyroWave) → WebGPU canvas
 * - video: WebTransport → WebCodecs → MediaStreamTrackGenerator → <video>
 * - canvas2d: WebTransport → WebCodecs → 2D canvas drawImage
 * - webrtc: RTP video track with playout-delay 0 → <video>
 */
type Path = "webgpu" | "video" | "canvas2d" | "webrtc";

interface Pct {
  p50: number | null;
  p95: number | null;
}

/** Absolute ms (timeOrigin + now) for one frame. */
interface FrameTimes {
  sentAt: number | null;
  /** Last fragment (WebTransport) or last packet (WebRTC receiveTime). */
  receivedAt?: number;
  /** WebCodecs output, PyroWave GPU done, or WebRTC receiveTime + processingDuration. */
  decodedAt?: number;
  /** GPU done after drawing (webgpu), drawImage returned (canvas2d). */
  drawnAt?: number;
  /** Handed to the compositor: the next rAF callback (canvas) or rVFC presentationTime (<video>). */
  presentedAt?: number;
  /** <video> only: rVFC expectedDisplayTime. */
  displayAt?: number;
}

interface Result {
  stream: string;
  codec: string;
  codecString: string | null;
  bitrateMbps: number;
  path: Path;
  decoder: string;
  framesReceived: number;
  framesDropped: number;
  framesShown: number;
  decodeErrors: number;
  lastFragmentMs: Pct;
  decodedMs: Pct;
  drawnMs: Pct;
  presentedMs: Pct;
  displayMs: Pct;
  decodeOnlyMs: Pct;
  minRttMs: number | null;
  server: Record<string, unknown> | null;
  /** WebRTC path: the receiver's inbound-rtp stats (decoder, jitter buffer, loss). */
  rtc?: Record<string, unknown> | null;
  endReason: string;
  /** First decoder error, or why the run failed. */
  error?: string | null;
  /** S2 streamer: synthetic click → the flash it causes on screen, by stage. */
  inputProbe?: Record<string, unknown>;
  /** WebRTC path: "probe" (synthetic clicks), "forward" (your input) or "none". */
  inputMode?: string;
  /** S2 streamer: request → first presented frame at each new size. */
  resizeTest?: Record<string, unknown>;
}

// Chrome's main-thread insertable stream for video (the standard is VideoTrackGenerator in workers).
declare class MediaStreamTrackGenerator extends MediaStreamTrack {
  constructor(init: { kind: "video" });
  readonly writable: WritableStream<VideoFrame>;
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const canvas = $<HTMLCanvasElement>("canvas");
const canvas2d = $<HTMLCanvasElement>("canvas2d");
const video = $<HTMLVideoElement>("video");
const now = () => performance.timeOrigin + performance.now();
const results: Result[] = [];
const env: Record<string, unknown> = { userAgent: navigator.userAgent };
let streams: StreamInfo[] = [];
let server: ServerInfo | null = null;
let httpBase = "";
let pw: PyroWaveDevice | null = null;
let ctx: GPUCanvasContext;
let yuv: YuvRenderer;
let videoRenderer: VideoFrameRenderer;
let busy = false;

async function setupGpu(): Promise<void> {
  const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new Error("WebGPU is not available");
  env.adapter = `${adapter.info.vendor} ${adapter.info.architecture}`;
  env.subgroups = adapter.features.has("subgroups");
  pw = await PyroWaveDevice.create(adapter, { timestamps: false });
  ctx = canvas.getContext("webgpu")!;
  const format = navigator.gpu.getPreferredCanvasFormat();
  ctx.configure({ device: pw.device, format, alphaMode: "opaque" });
  yuv = new YuvRenderer(pw.device, format);
  videoRenderer = new VideoFrameRenderer(pw.device, format);
}

/** WebCodecs config for a stream, preferring hardware; null for PyroWave. */
async function webCodecsConfig(s: StreamInfo): Promise<{ config: VideoDecoderConfig; hardware: boolean } | null> {
  if (s.codec === "pyrowave" || !s.codecString) return null;
  const base: VideoDecoderConfig = {
    codec: s.codecString,
    codedWidth: s.width,
    codedHeight: s.height,
    optimizeForLatency: true,
  };
  const hw = { ...base, hardwareAcceleration: "prefer-hardware" as const };
  if ((await VideoDecoder.isConfigSupported(hw)).supported) return { config: hw, hardware: true };
  const any = { ...base, hardwareAcceleration: "no-preference" as const };
  if ((await VideoDecoder.isConfigSupported(any)).supported) return { config: any, hardware: false };
  throw new Error(`${s.codecString} is not supported by this browser`);
}

const RTP_MIME: Record<string, string> = { h264: "video/h264", hevc: "video/h265", av1: "video/av1" };

function webRtcSupports(s: StreamInfo): boolean {
  const mime = RTP_MIME[s.codec];
  return !!mime && !!RTCRtpReceiver.getCapabilities("video")?.codecs.some((c) => c.mimeType.toLowerCase() === mime);
}

function pathsFor(s: StreamInfo): Path[] {
  if (server?.gateway) return webRtcSupports(s) ? ["webrtc"] : [];
  if (s.codec === "pyrowave") return ["webgpu"];
  const paths: Path[] = ["webgpu", "video", "canvas2d"];
  if (webRtcSupports(s)) paths.push("webrtc");
  return paths;
}

async function loadStreams(): Promise<void> {
  const host = $<HTMLInputElement>("host").value.trim();
  httpBase = `http://${host.includes(":") ? `[${host}]` : host}:${$<HTMLInputElement>("httpPort").value}`;
  server = (await (await fetch(`${httpBase}/info`)).json()) as ServerInfo;
  streams = (await (await fetch(`${httpBase}/streams`)).json()) as StreamInfo[];
  if (!pw) await setupGpu();
  const body = $("streams");
  body.innerHTML = "";
  const support: Record<string, string> = {};
  for (const s of streams) {
    let decoder: string;
    try {
      const wc = await webCodecsConfig(s);
      decoder = wc ? `WebCodecs (${wc.hardware ? "hardware" : "software?"})` : "WebGPU";
    } catch (err) {
      decoder = `unsupported: ${String(err)}`;
    }
    if (s.codec !== "pyrowave") decoder += webRtcSupports(s) ? ", WebRTC ok" : ", no WebRTC";
    support[s.name] = decoder;
    const tr = document.createElement("tr");
    for (const c of [s.name, s.codecString ?? s.codec, String(s.bitrateMbps), decoder]) {
      const td = document.createElement("td");
      td.textContent = c;
      tr.append(td);
    }
    const td = document.createElement("td");
    const run = document.createElement("button");
    run.textContent = "Run";
    run.onclick = () => guarded(async () => void (await runStream(s, selectedPath(s))));
    td.append(run);
    tr.append(td);
    body.append(tr);
  }
  env.decoders = support;
  $("env").textContent = JSON.stringify(env, null, 2);
  $<HTMLButtonElement>("all").disabled = false;
  $<HTMLButtonElement>("matrix").disabled = false;
  status(`Loaded ${streams.length} stream(s) from ${httpBase}.`);
}

function selectedPath(s: StreamInfo): Path {
  const path = $<HTMLSelectElement>("path").value as Path;
  const paths = pathsFor(s);
  return paths.includes(path) ? path : (paths[0] ?? "webgpu");
}

function showSurface(path: Path): void {
  canvas.hidden = path !== "webgpu";
  canvas2d.hidden = path !== "canvas2d";
  video.hidden = path !== "video" && path !== "webrtc";
}

async function runStream(s: StreamInfo, path: Path): Promise<Result> {
  if (!server || !pw) throw new Error("load streams first");
  status(`Running ${s.name} via ${path}…`);
  showSurface(path);
  try {
    return await (path === "webrtc" ? runWebRtc(s) : runWebTransport(s, path));
  } catch (err) {
    // Record the failure as a row and let a batch carry on.
    console.error(err);
    const message = String(err);
    const empty = { received: 0, dropped: 0, shown: 0, errors: 1 };
    return finish(s, path, path, new Map(), empty, { reason: "error", minRttMs: null, error: message }, null);
  }
}

/** Counts every presented frame of the <video> and records its rVFC times. */
function watchVideo(onFrame: (md: VideoFrameCallbackMetadata) => void): () => void {
  let running = true;
  const tick: VideoFrameRequestCallback = (_now, md) => {
    if (!running) return;
    onFrame(md);
    video.requestVideoFrameCallback(tick);
  };
  video.requestVideoFrameCallback(tick);
  return () => {
    running = false;
  };
}

async function runWebTransport(s: StreamInfo, path: Path): Promise<Result> {
  const device = pw!.device;
  const secs = Number($<HTMLInputElement>("secs").value) || 15;
  const fps = Number($<HTMLInputElement>("fps").value) || 60;
  canvas.width = canvas2d.width = s.width;
  canvas.height = canvas2d.height = s.height;

  const times = new Map<number, FrameTimes>();
  let received = 0;
  let dropped = 0;
  let shown = 0;
  let errors = 0;
  let firstError: string | null = null;
  let serverStats: Record<string, unknown> | null = null;

  // Canvas paths: the next animation-frame callback is the update that includes the draw.
  const onNextFrame = (t: FrameTimes) =>
    requestAnimationFrame(() => {
      t.presentedAt = now();
      shown++;
    });

  // <video> path: a track generator fed with decoded frames.
  let generatorWriter: WritableStreamDefaultWriter<VideoFrame> | null = null;
  let stopWatching: (() => void) | null = null;
  if (path === "video") {
    const generator = new MediaStreamTrackGenerator({ kind: "video" });
    generatorWriter = generator.writable.getWriter();
    video.srcObject = new MediaStream([generator]);
    void video.play();
    // mediaTime carries the VideoFrame timestamp, which is the frame id in µs units.
    stopWatching = watchVideo((md) => {
      const t = times.get(Math.round(md.mediaTime * 1e6));
      if (!t) return;
      t.presentedAt = performance.timeOrigin + md.presentationTime;
      t.displayAt = performance.timeOrigin + md.expectedDisplayTime;
      shown++;
    });
  }
  const ctx2d = path === "canvas2d" ? canvas2d.getContext("2d", { desynchronized: true })! : null;

  const present = (frame: VideoFrame, t: FrameTimes | undefined) => {
    if (path === "video") {
      void generatorWriter!.write(frame); // the sink closes the frame
      return;
    }
    if (path === "canvas2d") {
      ctx2d!.drawImage(frame, 0, 0, s.width, s.height);
      frame.close();
      if (t) {
        t.drawnAt = now();
        onNextFrame(t);
      }
      return;
    }
    const cmd = device.createCommandEncoder();
    videoRenderer.draw(cmd, ctx.getCurrentTexture().createView(), frame);
    device.queue.submit([cmd.finish()]);
    void device.queue.onSubmittedWorkDone().then(() => {
      frame.close();
      if (t) {
        t.drawnAt = now();
        onNextFrame(t);
      }
    });
  };

  const wc = await webCodecsConfig(s);
  let pyro: PyroWaveDecoder | null = null;
  let decoder: VideoDecoder | null = null;
  let colorSet = false;
  let sawKey = false;
  if (wc) {
    decoder = new VideoDecoder({
      output: (frame) => {
        const t = times.get(frame.timestamp);
        if (t) t.decodedAt = now();
        present(frame, t);
      },
      // WebCodecs closes the decoder after an error; later chunks are counted as errors.
      error: (e) => {
        errors++;
        firstError ??= `${e.name}: ${e.message}`;
        console.error(e);
      },
    });
    decoder.configure(wc.config);
  } else {
    pyro = new PyroWaveDecoder(pw!, s.width, s.height, s.chroma);
  }

  const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
  const finished = new Promise<{ reason: string; minRttMs: number | null }>((resolve, reject) => {
    worker.onerror = (e) => reject(new Error(e.message));
    worker.onmessage = (e: MessageEvent<WorkerMsg>) => {
      const msg = e.data;
      switch (msg.type) {
        case "frame": {
          received++;
          const t: FrameTimes = { sentAt: msg.sentAt, receivedAt: msg.lastAt };
          times.set(msg.id, t);
          const data = new Uint8Array(msg.data);
          if (pyro) {
            // Replayed streams restart their sequence numbers on every loop.
            pyro.clear();
            if (!pyro.pushPacket(data) || !pyro.isReady()) {
              errors++;
              break;
            }
            if (!colorSet) {
              yuv.setSource(pyro.planes, pyro.parser.color);
              colorSet = true;
            }
            const cmd = device.createCommandEncoder();
            pyro.encode(cmd);
            yuv.draw(cmd, ctx.getCurrentTexture().createView(), s.width, s.height);
            device.queue.submit([cmd.finish()]);
            void device.queue.onSubmittedWorkDone().then(() => {
              t.decodedAt = t.drawnAt = now();
              onNextFrame(t);
            });
          } else if (decoder) {
            if (!sawKey && !msg.key) break; // wait for the first keyframe
            sawKey = true;
            if (decoder.state === "closed") {
              errors++;
              break;
            }
            decoder.decode(new EncodedVideoChunk({ type: msg.key ? "key" : "delta", timestamp: msg.id, data }));
          }
          break;
        }
        case "dropped":
          dropped++;
          break;
        case "server":
          serverStats = msg.stats;
          break;
        case "done":
          resolve({ reason: msg.reason, minRttMs: msg.minRttMs });
          break;
        case "error":
          reject(new Error(msg.message));
          break;
        case "hello":
          break;
      }
    };
  });

  const start: StartMsg = {
    type: "start",
    host: $<HTMLInputElement>("host").value.trim(),
    wtPort: server!.wt_port,
    certHashHex: server!.cert_hash_hex,
    stream: s.name,
    fps,
    secs,
    dgram: Number($<HTMLInputElement>("dgram").value) || 1200,
  };
  worker.postMessage(start);
  let end: { reason: string; minRttMs: number | null };
  try {
    end = await finished;
  } finally {
    worker.terminate();
  }
  if (decoder?.state === "configured") await decoder.flush().catch(() => {});
  await device.queue.onSubmittedWorkDone();
  await new Promise((r) => setTimeout(r, 200));
  stopWatching?.();
  if (decoder && decoder.state !== "closed") decoder.close();
  pyro?.destroy();
  await generatorWriter?.close().catch(() => {});
  video.srcObject = null;

  const label = wc ? `${path} (WebCodecs ${wc.hardware ? "hw" : "sw?"})` : `${path} (PyroWave)`;
  return finish(s, path, label, times, { received, dropped, shown, errors }, { ...end, error: firstError }, serverStats);
}

/**
 * Sends pointer, wheel and keys over the control channel while the pointer is
 * over the video (S2 streamer). Positions are normalized to the video picture
 * (letterboxing excluded); keys are physical `KeyboardEvent.code`s.
 */
function forwardInput(control: RTCDataChannel): () => void {
  const send = (msg: Record<string, unknown>) => {
    if (control.readyState === "open") control.send(JSON.stringify({ t: "input", ...msg }) + "\n");
  };
  const toVideo = (e: PointerEvent | WheelEvent) => {
    const r = video.getBoundingClientRect();
    const scale = Math.min(r.width / (video.videoWidth || r.width), r.height / (video.videoHeight || r.height));
    const w = (video.videoWidth || r.width) * scale;
    const h = (video.videoHeight || r.height) * scale;
    return { x: (e.clientX - r.left - (r.width - w) / 2) / w, y: (e.clientY - r.top - (r.height - h) / 2) / h };
  };
  let over = false;
  const move = (e: PointerEvent) => send({ k: "move", ...toVideo(e) });
  const down = (e: PointerEvent) => {
    video.setPointerCapture(e.pointerId);
    send({ k: "move", ...toVideo(e) });
    send({ k: "button", b: e.button, down: true });
    e.preventDefault();
  };
  const up = (e: PointerEvent) => send({ k: "button", b: e.button, down: false });
  const wheel = (e: WheelEvent) => {
    send({ k: "wheel", dx: e.deltaX, dy: e.deltaY });
    e.preventDefault();
  };
  const key = (down: boolean) => (e: KeyboardEvent) => {
    if (!over) return;
    send({ k: "key", code: e.code, down });
    e.preventDefault();
  };
  const keydown = key(true);
  const keyup = key(false);
  const enter = () => (over = true);
  const leave = () => (over = false);
  const menu = (e: Event) => e.preventDefault();
  video.style.cursor = "none";
  video.addEventListener("pointermove", move);
  video.addEventListener("pointerdown", down);
  video.addEventListener("pointerup", up);
  video.addEventListener("pointerenter", enter);
  video.addEventListener("pointerleave", leave);
  video.addEventListener("wheel", wheel, { passive: false });
  video.addEventListener("contextmenu", menu);
  addEventListener("keydown", keydown);
  addEventListener("keyup", keyup);
  return () => {
    video.style.cursor = "";
    video.removeEventListener("pointermove", move);
    video.removeEventListener("pointerdown", down);
    video.removeEventListener("pointerup", up);
    video.removeEventListener("pointerenter", enter);
    video.removeEventListener("pointerleave", leave);
    video.removeEventListener("wheel", wheel);
    video.removeEventListener("contextmenu", menu);
    removeEventListener("keydown", keydown);
    removeEventListener("keyup", keyup);
  };
}

const PROBE_INTERVAL_MS = 500;
const FLASH_LUMA = 200;

interface ProbeSample {
  id: number;
  /** Page clock (absolute ms) when the click went out. */
  clickAt: number;
  /** Streamer clock (µs) when the click arrived. */
  srvUs?: number;
  /** First frame showing the flash. */
  frameId?: number;
  presentedAt?: number;
  displayAt?: number;
}

/**
 * Input → screen round trip on the S2 streamer: a synthetic click every
 * PROBE_INTERVAL_MS; the live page in the stream turns white on each click;
 * the first presented frame whose bottom-right corner is white closes it.
 */
class InputProbe {
  readonly samples: ProbeSample[] = [];
  private pending: ProbeSample | null = null;
  private startTimer?: ReturnType<typeof setTimeout>;
  private clickTimer?: ReturnType<typeof setInterval>;
  private readonly ctx: CanvasRenderingContext2D;

  constructor(private readonly control: RTCDataChannel) {
    const c = document.createElement("canvas");
    c.width = c.height = 4;
    this.ctx = c.getContext("2d", { willReadFrequently: true })!;
  }

  start(afterMs: number): void {
    // Park the remote pointer in the middle, away from the sampled corner.
    this.send({ k: "move", x: 0.5, y: 0.5 });
    this.startTimer = setTimeout(() => (this.clickTimer = setInterval(() => this.click(), PROBE_INTERVAL_MS)), afterMs);
  }

  stop(): void {
    clearTimeout(this.startTimer);
    clearInterval(this.clickTimer);
  }

  acknowledged(id: number, srvUs: number): void {
    const s = this.samples[id];
    if (s) s.srvUs = srvUs;
  }

  onFrame(md: VideoFrameCallbackMetadata, frameId: number | undefined): void {
    const s = this.pending;
    const w = video.videoWidth;
    const h = video.videoHeight;
    if (!s || !w) return;
    this.ctx.drawImage(video, w * 0.9, h * 0.9, w * 0.06, h * 0.06, 0, 0, 4, 4);
    const d = this.ctx.getImageData(0, 0, 4, 4).data;
    let luma = 0;
    for (let i = 0; i < d.length; i += 4) luma += 0.2126 * d[i]! + 0.7152 * d[i + 1]! + 0.0722 * d[i + 2]!;
    if (luma / 16 < FLASH_LUMA) return;
    s.frameId = frameId;
    s.presentedAt = performance.timeOrigin + md.presentationTime;
    s.displayAt = performance.timeOrigin + md.expectedDisplayTime;
    this.pending = null;
  }

  /** Per-stage p50/p95, with the final clock offset. */
  summary(
    toAbs: (us: number) => number | null,
    sendUs: Map<number, number>,
    frameUs: Map<number, { c?: number; e?: number }>,
  ): Record<string, unknown> {
    const stage = (f: (s: ProbeSample) => number | null) =>
      pct(this.samples.flatMap((s) => (s.presentedAt === undefined ? [] : [f(s)].filter((v): v is number => v !== null))));
    const abs = (us: number | undefined) => (us === undefined ? null : toAbs(us));
    const frame = (s: ProbeSample) => (s.frameId === undefined ? undefined : frameUs.get(s.frameId));
    const sent = (s: ProbeSample) => abs(s.frameId === undefined ? undefined : sendUs.get(s.frameId));
    const minus = (a: number | null | undefined, b: number | null | undefined) =>
      a === null || a === undefined || b === null || b === undefined ? null : a - b;
    return {
      n: this.samples.filter((s) => s.presentedAt !== undefined).length,
      missed: this.samples.filter((s) => s.presentedAt === undefined).length,
      intervalMs: PROBE_INTERVAL_MS,
      totalMs: stage((s) => s.presentedAt! - s.clickAt),
      toDisplayMs: stage((s) => minus(s.displayAt, s.clickAt)),
      stagesMs: {
        clickToStreamer: stage((s) => minus(abs(s.srvUs), s.clickAt)),
        streamerToComposited: stage((s) => minus(abs(frame(s)?.c), abs(s.srvUs))),
        compositedToEncoded: stage((s) => minus(abs(frame(s)?.e), abs(frame(s)?.c))),
        encodedToSent: stage((s) => minus(sent(s), abs(frame(s)?.e))),
        sentToPresented: stage((s) => minus(s.presentedAt, sent(s))),
      },
    };
  }

  private click(): void {
    if (this.control.readyState !== "open") return;
    this.pending = { id: this.samples.length, clickAt: performance.timeOrigin + performance.now() };
    this.samples.push(this.pending);
    this.send({ k: "button", b: 0, down: true, probe: this.pending.id });
    this.send({ k: "button", b: 0, down: false });
  }

  private send(msg: Record<string, unknown>): void {
    if (this.control.readyState === "open") this.control.send(JSON.stringify({ t: "input", ...msg }) + "\n");
  }
}

const RESIZE_STEPS: [number, number][] = [[1920, 1080], [1280, 720], [2560, 1440]];
const RESIZE_EVERY_MS = 3000;

interface ResizeStep {
  w: number;
  h: number;
  /** Page clock (absolute ms) when the request went out. */
  askedAt: number;
  /** First presented frame at the new size. */
  presentedAt?: number;
}

/**
 * Resizes the S2 streamer's output mid-run and times request → first presented
 * frame at each new size, plus the longest gap between presented frames.
 */
class ResizeTest {
  readonly steps: ResizeStep[] = [];
  private readonly timers: ReturnType<typeof setTimeout>[] = [];
  private readonly presented: number[] = [];

  constructor(private readonly control: RTCDataChannel) {}

  start(): void {
    RESIZE_STEPS.forEach(([w, h], i) => {
      this.timers.push(
        setTimeout(() => {
          if (this.control.readyState !== "open") return;
          this.steps.push({ w, h, askedAt: performance.timeOrigin + performance.now() });
          this.control.send(JSON.stringify({ t: "resize", w, h }) + "\n");
        }, RESIZE_EVERY_MS * (i + 1)),
      );
    });
  }

  stop(): void {
    this.timers.forEach(clearTimeout);
  }

  onFrame(md: VideoFrameCallbackMetadata): void {
    const at = performance.timeOrigin + md.presentationTime;
    this.presented.push(at);
    const step = this.steps.at(-1);
    if (step && step.presentedAt === undefined && md.width === step.w && md.height === step.h) step.presentedAt = at;
  }

  summary(): Record<string, unknown> {
    return {
      steps: this.steps.map((s) => {
        const around = this.presented.filter((t) => t >= s.askedAt - 500 && t <= s.askedAt + 1500);
        const gaps = around.slice(1).map((t, i) => t - around[i]!);
        return {
          size: `${s.w}x${s.h}`,
          toPresentedMs: s.presentedAt === undefined ? null : +(s.presentedAt - s.askedAt).toFixed(1),
          maxPresentGapMs: gaps.length ? +Math.max(...gaps).toFixed(1) : null,
        };
      }),
    };
  }
}

/** The RTP path: str0m sends the stream as a video track; rVFC reports each frame. */
async function runWebRtc(s: StreamInfo): Promise<Result> {
  const secs = Number($<HTMLInputElement>("secs").value) || 15;
  const fps = Number($<HTMLInputElement>("fps").value) || 60;
  const host = $<HTMLInputElement>("host").value.trim();
  const times = new Map<number, FrameTimes>();
  const idByRtp = new Map<number, number>();
  const sendUs = new Map<number, number>();
  const frameUs = new Map<number, { c?: number; e?: number }>();
  let shown = 0;
  let serverStats: Record<string, unknown> | null = null;
  let best: { rtt: number; offset: number } | null = null;
  let probe: InputProbe | null = null;
  let resizer: ResizeTest | null = null;
  const toAbs = (us: number) => (best ? performance.timeOrigin + us / 1000 - best.offset : null);

  const pc = new RTCPeerConnection();
  const control = pc.createDataChannel("control");
  const transceiver = pc.addTransceiver("video", { direction: "recvonly" });
  const mime = RTP_MIME[s.codec]!;
  const codecs = RTCRtpReceiver.getCapabilities("video")!.codecs.filter((c) => c.mimeType.toLowerCase() === mime);
  transceiver.setCodecPreferences(codecs);
  pc.ontrack = (e) => {
    try {
      e.receiver.jitterBufferTarget = 0;
    } catch {
      // Not supported here.
    }
    video.srcObject = new MediaStream([e.track]);
    void video.play();
  };

  const stopWatching = watchVideo((md) => {
    if (md.rtpTimestamp === undefined) return;
    const id = idByRtp.get(md.rtpTimestamp);
    probe?.onFrame(md, id);
    resizer?.onFrame(md);
    if (id === undefined) return;
    const t = times.get(id) ?? { sentAt: null };
    times.set(id, t);
    if (md.receiveTime !== undefined) {
      t.receivedAt = performance.timeOrigin + md.receiveTime;
      if (md.processingDuration !== undefined) t.decodedAt = t.receivedAt + md.processingDuration * 1000;
    }
    t.presentedAt = performance.timeOrigin + md.presentationTime;
    t.displayAt = performance.timeOrigin + md.expectedDisplayTime;
    shown++;
  });

  let pinger: ReturnType<typeof setInterval> | undefined;
  let stopInput: (() => void) | null = null;
  const done = new Promise<string>((resolve) => {
    let buffer = "";
    control.onopen = () => {
      const ping = () => control.readyState === "open" && control.send(JSON.stringify({ t: "ping", c: performance.now() }) + "\n");
      ping();
      pinger = setInterval(ping, 200);
      if (server?.resize && $<HTMLInputElement>("resizeTest").checked) {
        resizer = new ResizeTest(control);
        resizer.start();
      }
      if (server?.input && $<HTMLInputElement>("probe").checked) {
        probe = new InputProbe(control);
        probe.start(2000);
        status(`Running ${s.name} via webrtc, input probe on (hands off the video)…`);
      } else if (server?.input) {
        stopInput = forwardInput(control);
        status(`Running ${s.name} via webrtc, forwarding your input over the video…`);
      }
    };
    control.onmessage = (e: MessageEvent<string>) => {
      buffer += e.data;
      let nl: number;
      while ((nl = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, nl).trim();
        buffer = buffer.slice(nl + 1);
        if (!line) continue;
        const msg = JSON.parse(line) as {
          t: string;
          c?: number;
          s_us?: number;
          id?: number;
          rtp?: number;
          c_us?: number;
          e_us?: number;
        };
        if (msg.t === "pong") {
          const tNow = performance.now();
          const rtt = tNow - msg.c!;
          if (!best || rtt < best.rtt) best = { rtt, offset: msg.s_us! / 1000 - (msg.c! + tNow) / 2 };
        } else if (msg.t === "sent") {
          idByRtp.set(msg.rtp!, msg.id!);
          sendUs.set(msg.id!, msg.s_us!);
          if (msg.e_us !== undefined) frameUs.set(msg.id!, { c: msg.c_us, e: msg.e_us });
        } else if (msg.t === "probe") {
          probe?.acknowledged(msg.id!, msg.s_us!);
        } else if (msg.t === "stats" || msg.t === "done") {
          serverStats = msg as Record<string, unknown>;
          if (msg.t === "done") setTimeout(() => resolve("done"), 600);
        }
      }
    };
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === "failed" || pc.connectionState === "closed") resolve(`connection ${pc.connectionState}`);
    };
    setTimeout(() => resolve("timeout"), (secs + 15) * 1000);
  });

  await pc.setLocalDescription(await pc.createOffer());
  const token = $<HTMLInputElement>("token").value.trim();
  const query = `name=${encodeURIComponent(s.name)}&fps=${fps}&secs=${secs}&host=${encodeURIComponent(host)}&token=${encodeURIComponent(token)}`;
  const res = await fetch(`${httpBase}/webrtc/media?${query}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(pc.localDescription),
  });
  if (!res.ok) throw new Error(`offer rejected: ${res.status} ${await res.text()}`);
  await pc.setRemoteDescription((await res.json()) as RTCSessionDescriptionInit);

  const reason = await done;
  clearInterval(pinger);
  // Assigned in control.onopen, which TypeScript's narrowing can't see.
  (stopInput as (() => void) | null)?.();
  (probe as InputProbe | null)?.stop();
  (resizer as ResizeTest | null)?.stop();
  stopWatching();
  const minRttMs = best ? (best as { rtt: number }).rtt : null;
  // Map send times with the final (best) clock offset.
  for (const [id, us] of sendUs) {
    const t = times.get(id) ?? { sentAt: null };
    t.sentAt = toAbs(us);
    times.set(id, t);
  }
  const rtc = await inboundVideoStats(pc);
  pc.close();
  video.srcObject = null;

  const counts = { received: sendUs.size, dropped: 0, shown, errors: 0 };
  const impl = rtc?.decoderImplementation ?? "browser decoder";
  const label = `webrtc (RTP, ${String(impl)}${rtc?.powerEfficientDecoder ? ", hw" : ""})`;
  const result = finish(s, "webrtc", label, times, counts, { reason, minRttMs }, serverStats, rtc);
  const p = probe as InputProbe | null;
  result.inputMode = p ? "probe" : server?.input ? "forward" : "none";
  const r = resizer as ResizeTest | null;
  if (r) result.resizeTest = r.summary();
  if (p) {
    result.inputProbe = p.summary(toAbs, sendUs, frameUs);
    const total = result.inputProbe.totalMs as Pct;
    status(`Input → screen: p50 ${total.p50?.toFixed(1)} ms, p95 ${total.p95?.toFixed(1)} ms over ${String(result.inputProbe.n)} clicks`);
  }
  return result;
}

/** The video receiver's inbound-rtp stats, plus per-frame averages in ms. */
async function inboundVideoStats(pc: RTCPeerConnection): Promise<Record<string, unknown> | null> {
  const keys = [
    "decoderImplementation", "powerEfficientDecoder", "framesReceived", "framesDecoded", "framesDropped",
    "keyFramesDecoded", "framesPerSecond", "frameWidth", "frameHeight", "packetsLost", "nackCount", "pliCount",
    "firCount", "totalDecodeTime", "totalProcessingDelay", "totalAssemblyTime", "jitterBufferDelay",
    "jitterBufferTargetDelay", "jitterBufferMinimumDelay", "jitterBufferEmittedCount",
  ];
  for (const report of (await pc.getStats()).values()) {
    if (report.type !== "inbound-rtp" || report.kind !== "video") continue;
    const r = report as Record<string, number | string | boolean | undefined>;
    const out: Record<string, unknown> = Object.fromEntries(keys.filter((k) => r[k] !== undefined).map((k) => [k, r[k]]));
    const per = (total: string, count: string) =>
      typeof r[total] === "number" && typeof r[count] === "number" && (r[count] as number) > 0
        ? +(((r[total] as number) / (r[count] as number)) * 1000).toFixed(2)
        : null;
    out.decodeMsAvg = per("totalDecodeTime", "framesDecoded");
    out.processingMsAvg = per("totalProcessingDelay", "framesDecoded");
    out.jitterBufferMsAvg = per("jitterBufferDelay", "jitterBufferEmittedCount");
    return out;
  }
  return null;
}

function finish(
  s: StreamInfo,
  path: Path,
  decoderLabel: string,
  times: Map<number, FrameTimes>,
  counts: { received: number; dropped: number; shown: number; errors: number },
  end: { reason: string; minRttMs: number | null; error?: string | null },
  serverStats: Record<string, unknown> | null,
  rtc: Record<string, unknown> | null = null,
): Result {
  const all = [...times.values()];
  const since = (pick: (t: FrameTimes) => number | undefined) =>
    pct(all.flatMap((t) => (t.sentAt !== null && pick(t) !== undefined ? [pick(t)! - t.sentAt] : [])));
  const result: Result = {
    stream: s.name,
    codec: s.codec,
    codecString: s.codecString,
    bitrateMbps: s.bitrateMbps,
    path,
    decoder: decoderLabel,
    framesReceived: counts.received,
    framesDropped: counts.dropped,
    framesShown: counts.shown,
    decodeErrors: counts.errors,
    lastFragmentMs: since((t) => t.receivedAt),
    decodedMs: since((t) => t.decodedAt),
    drawnMs: since((t) => t.drawnAt),
    presentedMs: since((t) => t.presentedAt),
    displayMs: since((t) => t.displayAt),
    decodeOnlyMs: pct(
      all.flatMap((t) => (t.decodedAt !== undefined && t.receivedAt !== undefined ? [t.decodedAt - t.receivedAt] : [])),
    ),
    minRttMs: end.minRttMs,
    server: serverStats,
    rtc,
    endReason: end.reason,
    error: end.error ?? null,
  };
  results.push(result);
  render(result);
  status(`Finished ${s.name} via ${path} (${end.reason}).`);
  return result;
}

function pct(values: number[]): Pct {
  if (!values.length) return { p50: null, p95: null };
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q: number) => sorted[Math.min(sorted.length - 1, Math.ceil(q * sorted.length) - 1)]!;
  return { p50: at(0.5), p95: at(0.95) };
}

function render(r: Result): void {
  const f = (v: number | null) => (v === null ? "–" : v.toFixed(1));
  const pair = (p: Pct) => `${f(p.p50)} / ${f(p.p95)}`;
  const tr = document.createElement("tr");
  const ok = r.framesReceived ? `${r.framesShown} / ${r.framesReceived + r.framesDropped}` : "–";
  const cells = [
    r.stream,
    r.codecString ?? r.codec,
    String(r.bitrateMbps),
    r.error ? `${r.decoder}: ${r.error}` : r.decoder,
    ok,
    pair(r.lastFragmentMs),
    pair(r.decodedMs),
    pair(r.drawnMs),
    pair(r.presentedMs),
    pair(r.displayMs),
    pair(r.decodeOnlyMs),
  ];
  for (const c of cells) {
    const td = document.createElement("td");
    td.textContent = c;
    tr.append(td);
  }
  if (r.decodeErrors || r.error) {
    tr.title = [r.decodeErrors ? `${r.decodeErrors} decode errors` : "", r.error ?? ""].filter(Boolean).join("; ");
    tr.classList.add("warn");
  }
  $("results").append(tr);
}

async function guarded(task: () => Promise<void>): Promise<void> {
  if (busy) return;
  busy = true;
  document.querySelectorAll("button").forEach((b) => (b.disabled = b.id !== "copy"));
  try {
    await task();
  } catch (err) {
    console.error(err);
    status(`Error: ${String(err)}`);
  } finally {
    busy = false;
    document.querySelectorAll("button").forEach((b) => (b.disabled = false));
  }
}

function status(text: string): void {
  $("status").textContent = text;
}

const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Keeps the setup across reloads: fields in localStorage, the token only for this tab's session. */
function persistSetup(): void {
  const fields = ["host", "httpPort", "secs", "fps", "dgram", "path", "probe", "resizeTest", "token"];
  const store = (id: string) => (id === "token" ? sessionStorage : localStorage);
  for (const id of fields) {
    const el = document.getElementById(id) as HTMLInputElement | HTMLSelectElement | null;
    if (!el) continue;
    const key = `s1c-setup-${id}`;
    try {
      const saved = store(id).getItem(key);
      if (saved !== null) {
        if (el instanceof HTMLInputElement && el.type === "checkbox") el.checked = saved === "1";
        else el.value = saved;
      }
    } catch {
      // Storage unavailable; the form just starts empty.
    }
    el.addEventListener("change", () => {
      try {
        const value = el instanceof HTMLInputElement && el.type === "checkbox" ? (el.checked ? "1" : "0") : el.value;
        store(id).setItem(key, value);
      } catch {
        // Ignore: persistence is a convenience.
      }
    });
  }
}
persistSetup();

$("load").onclick = () => guarded(loadStreams);
$("all").onclick = () =>
  guarded(async () => {
    for (const s of streams) {
      await runStream(s, selectedPath(s));
      await pause(1500);
    }
    status("All streams done. Use “Copy results JSON” to save them.");
  });
$("matrix").onclick = () =>
  guarded(async () => {
    const picks = server?.gateway ? streams.map((x) => x.name) : ["h264-420-40m", "hevc-420-40m", "av1-420-40m"];
    for (const name of picks) {
      const s = streams.find((x) => x.name === name);
      if (!s) continue;
      for (const path of pathsFor(s)) {
        await runStream(s, path);
        await pause(1500);
      }
    }
    status("Matrix done. Use “Copy results JSON” to save it.");
  });
$("copy").onclick = async () => {
  await navigator.clipboard.writeText(JSON.stringify({ capturedAt: new Date().toISOString(), env, results }, null, 2));
  status(`Copied ${results.length} result(s).`);
};
