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

/** The RTP path: str0m sends the stream as a video track; rVFC reports each frame. */
async function runWebRtc(s: StreamInfo): Promise<Result> {
  const secs = Number($<HTMLInputElement>("secs").value) || 15;
  const fps = Number($<HTMLInputElement>("fps").value) || 60;
  const host = $<HTMLInputElement>("host").value.trim();
  const times = new Map<number, FrameTimes>();
  const idByRtp = new Map<number, number>();
  const sendUs = new Map<number, number>();
  let shown = 0;
  let serverStats: Record<string, unknown> | null = null;
  let best: { rtt: number; offset: number } | null = null;
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
  const done = new Promise<string>((resolve) => {
    let buffer = "";
    control.onopen = () => {
      const ping = () => control.readyState === "open" && control.send(JSON.stringify({ t: "ping", c: performance.now() }) + "\n");
      ping();
      pinger = setInterval(ping, 200);
    };
    control.onmessage = (e: MessageEvent<string>) => {
      buffer += e.data;
      let nl: number;
      while ((nl = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, nl).trim();
        buffer = buffer.slice(nl + 1);
        if (!line) continue;
        const msg = JSON.parse(line) as { t: string; c?: number; s_us?: number; id?: number; rtp?: number };
        if (msg.t === "pong") {
          const tNow = performance.now();
          const rtt = tNow - msg.c!;
          if (!best || rtt < best.rtt) best = { rtt, offset: msg.s_us! / 1000 - (msg.c! + tNow) / 2 };
        } else if (msg.t === "sent") {
          idByRtp.set(msg.rtp!, msg.id!);
          sendUs.set(msg.id!, msg.s_us!);
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
  const query = `name=${encodeURIComponent(s.name)}&fps=${fps}&secs=${secs}&host=${encodeURIComponent(host)}`;
  const res = await fetch(`${httpBase}/webrtc/media?${query}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(pc.localDescription),
  });
  if (!res.ok) throw new Error(`offer rejected: ${res.status} ${await res.text()}`);
  await pc.setRemoteDescription((await res.json()) as RTCSessionDescriptionInit);

  const reason = await done;
  clearInterval(pinger);
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
  return finish(s, "webrtc", label, times, counts, { reason, minRttMs }, serverStats, rtc);
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
