// One connection to an environment's streamer, over one of two transports:
// - WebTransport (`cha-stream/1`, the Chromium fast path): video and audio
//   datagrams reassembled in a worker, decoded with WebCodecs and shown through
//   track generators (S1d: the fastest way to the screen);
// - WebRTC everywhere else: recvonly video (playout-delay 0) and stereo Opus.
// Video and audio are separate MediaStreams, so video never waits for lip
// sync. The control channel (a DataChannel, or WebTransport's first stream)
// carries input, resize requests and clock pings up, and per-frame send times
// down. The portal brokers the session; media flows straight from the node.

import { ControllerManager, type ManagedController } from "./controllers";
import { InputCapture } from "./input";
import { ClickProbe, percentile, type ProbeResult } from "./probe";
import { NODE_STATS_FRESH_MS, StatsReader, toNodeStats, type NodeStats, type StatsSnapshot } from "./stats";
import { PyroPresenter } from "./pyro";
import type { FromWorker, ToWorker } from "./wt-worker";

/** Hardware codecs (every transport) and PyroWave (WebTransport only). */
export type Codec = "hevc" | "h264" | "av1" | "pyrowave420" | "pyrowave444";

export function isPyroWave(codec: Codec): codec is "pyrowave420" | "pyrowave444" {
  return codec === "pyrowave420" || codec === "pyrowave444";
}
export type PlayerState = "idle" | "connecting" | "connected" | "disconnected" | "failed";

export type Transport = "webrtc" | "webtransport";

type HwCodec = "hevc" | "h264" | "av1";
const MIME: Record<HwCodec, string> = { hevc: "video/h265", h264: "video/h264", av1: "video/av1" };
/** WebCodecs names for what NVENC sends (Annex B, parameter sets in band). */
const WEBCODECS: Record<HwCodec, string> = { hevc: "hev1.1.6.L153.B0", h264: "avc1.640033", av1: "av01.0.13M.08" };

// Chromium's main-thread insertable stream (the standard is VideoTrackGenerator, in workers).
declare class MediaStreamTrackGenerator<T> extends MediaStreamTrack {
  constructor(init: { kind: "video" | "audio" });
  readonly writable: WritableStream<T>;
}

/** WebTransport with WebCodecs and track generators: Chromium. */
export function supportsWebTransport(): boolean {
  return (
    typeof WebTransport !== "undefined" &&
    typeof VideoDecoder !== "undefined" &&
    typeof AudioDecoder !== "undefined" &&
    "MediaStreamTrackGenerator" in globalThis
  );
}

/** Where to reach the streamer over WebTransport (from the portal). */
export interface WebTransportOffer {
  urls: string[];
  certHash: string;
}
/** Fastest to the screen first, as measured on the baseline (P1.3): HEVC
 *  decodes ~0.6 ms faster than H.264 in Chrome on Apple silicon, AV1 ~1.9 ms
 *  slower. */
const PREFERENCE: HwCodec[] = ["hevc", "h264", "av1"];

/** The hardware codecs this browser can receive over WebRTC, best first. */
export function supportedCodecs(): Codec[] {
  const caps = RTCRtpReceiver.getCapabilities?.("video");
  if (!caps) return ["h264"];
  const have = new Set(caps.codecs.map((c) => c.mimeType.toLowerCase()));
  return PREFERENCE.filter((c) => have.has(MIME[c]));
}

export interface PlayerOptions {
  video: HTMLVideoElement;
  /** Carries the offer to the environment (through the portal); returns the answer. */
  signal: (offer: RTCSessionDescriptionInit, codec: Codec) => Promise<RTCSessionDescriptionInit>;
  codec?: Codec;
  /** STUN/TURN servers (the portal mints TURN credentials). Without them,
   *  only direct paths: LAN, mesh, or a port-forwarded node. */
  iceServers?: RTCIceServer[];
  /** The largest picture to ask for (default 2560×1440, the baseline). */
  maxSize?: { width: number; height: number };
  /** The app's display has a fixed size: never ask for a resize, so the
   *  browser letterboxes the picture instead. */
  fixedSize?: boolean;
  onState?: (state: PlayerState, detail?: string) => void;
  /** Start with the sound off. */
  muted?: boolean;
  /** Sound's level, 0..1 (default 1). */
  volume?: number;
  /** Sound is waiting for a click or key press (the browser's autoplay rule). */
  onAudioBlocked?: (blocked: boolean) => void;
  /** Asks the portal for the streamer's WebTransport URLs; without it, WebRTC only. */
  webTransport?: (codec: Codec) => Promise<WebTransportOffer>;
  /** "auto" (the default): WebTransport where the browser has it, else WebRTC. */
  transport?: "auto" | Transport;
  /** The transport in use, once connected. */
  onTransport?: (transport: Transport) => void;
  /**
   * An app copied text, already written to this device's clipboard when the
   * browser allows (`written`); otherwise it's written on the next click.
   */
  onClipboard?: (text: string, written: boolean) => void;
  /**
   * Whether this page has the controls (only one of the sessions watching an
   * environment does), and how many sessions watch. Without the controls the
   * picture is view-only; `takeControl()` asks for them.
   */
  onFloor?: (control: boolean, viewers: number) => void;
  /**
   * What the app's long setup is doing while its picture may still be black
   * (a first-run download), or `null` once there is nothing to show. Every
   * session gets it, watching ones too; a late joiner is told at once.
   */
  onStatus?: (status: SetupStatus | null) => void;
  /**
   * The frame rate the environment encodes at (60, 90 or 120), when the
   * streamer first says and whenever it changes, whoever changed it.
   */
  onFps?: (fps: number) => void;
  /** The controllers this page sends (Gamepad API or WebHID), now and whenever one arrives or leaves. */
  onControllers?: (controllers: ManagedController[]) => void;
}

/**
 * An app's setup progress: a label, and how far along when it knows. With no
 * `total` the progress is indeterminate; `unit` names `done` and `total`.
 */
export interface SetupStatus {
  label: string;
  done?: number;
  total?: number;
  unit?: string;
}

interface ServerMessage {
  t: string;
  /** A probe's id (number), or a cursor image's (string). */
  id?: number | string;
  rtp?: number;
  s_us?: number;
  c?: number;
  codec?: string;
  stream?: number;
  error?: string;
  text?: string;
  kind?: "hidden" | "named" | "image";
  control?: boolean;
  viewers?: number;
  drawn?: boolean;
  name?: string;
  w?: number;
  h?: number;
  x?: number;
  y?: number;
  rgba?: string;
  label?: string;
  done?: number;
  total?: number;
  unit?: string;
  /** Rumble: the pad's slot, the strong and weak motors (0..1) and how long (0 stops). */
  i?: number;
  lo?: number;
  hi?: number;
  ms?: number;
  /** Haptic: the trackpad, its strength, and the pulse train; led: colour; players: LED bits; trigger: which trigger and its 11 bytes. */
  side?: string;
  amp?: number;
  on_us?: number;
  off_us?: number;
  count?: number;
  r?: number;
  g?: number;
  b?: number;
  mask?: number;
  effect?: unknown;
  /** Fps: the frame rate now; stats carry it too. */
  fps?: number;
  /** Cumulative frames the streamer sent (`stats`). */
  frames_sent?: number;
}

/** A status message as a status: none without a label, and only the numbers it has. */
function toStatus(msg: ServerMessage): SetupStatus | null {
  if (typeof msg.label !== "string" || !msg.label) return null;
  const status: SetupStatus = { label: msg.label };
  if (typeof msg.done === "number") status.done = msg.done;
  if (typeof msg.total === "number" && msg.total > 0) status.total = msg.total;
  if (typeof msg.unit === "string" && msg.unit) status.unit = msg.unit;
  return status;
}

/** A cursor image from the environment, ready for CSS. */
interface CursorImage {
  url: string;
  x: number;
  y: number;
}

/** How one video stream (one codec) is decoded on the WebTransport path. */
interface VideoPipeline {
  codec: Codec;
  /** PyroWave: WebGPU. */
  pyro: PyroPresenter | null;
  /** The hardware codecs: WebCodecs. */
  video: VideoDecoder | null;
  /** Until a keyframe, a (new) decoder can't take anything. */
  sawKey: boolean;
  closed: boolean;
}

/** How long a codec switch may take before the player reconnects instead. */
const SWITCH_TIMEOUT_MS = 3000;
/** How long a frame rate change may take to be answered. */
const FPS_TIMEOUT_MS = 3000;
/** How far back the send rate looks: a few of the streamer's reports. */
const SENT_WINDOW_MS = 4000;

/** The frame rates a page may ask the streamer for. */
export const FRAME_RATES = [60, 90, 120] as const;
export type FrameRate = (typeof FRAME_RATES)[number];

export class Player {
  state: PlayerState = "idle";
  /** The video codec; `switchCodec()` changes it. */
  codec: Codec;
  private pc: RTCPeerConnection | null = null;
  private control: RTCDataChannel | null = null;
  private input: InputCapture | null = null;
  private pads: ControllerManager | null = null;
  private probe: ClickProbe | null = null;
  private readonly stats = new StatsReader();
  private readonly cleanup: (() => void)[] = [];
  /** Local clock − server clock (ms), from the lowest-RTT ping so far. */
  private offset: { rtt: number; ms: number } | null = null;
  /** RTP timestamp → when the server sent that frame (server µs). */
  private readonly sentAt = new Map<number, number>();
  /** Server send → presented here (ms), recent frames. */
  private readonly latencies: number[] = [];
  /** WebRTC: frames' send → complete (their last packet in), for the streamer's rate control. */
  private readonly rtcDelivery: { at: number; ms: number }[] = [];
  private lastSize = "";
  /** The frame rate the streamer encodes at, once it has said. */
  private streamFps: number | null = null;
  /** A frame rate change asked for, waiting for the streamer's answer. */
  private fpsChange: { done: (fps: number) => void; fail: (err: Error) => void } | null = null;
  /** This page has the controls (streamers before P2.6 don't say: assume so). */
  private hasControl = true;
  /** Where a viewer's page draws the controller's pointer. */
  private pointerEl: HTMLElement | null = null;
  /** The node's latest resource report, and when it came. */
  /** The streamer's cumulative `frames_sent` at each recent `stats` message, for the send rate. */
  private sentCounts: { at: number; frames: number }[] = [];
  private nodeStats: { stats: Omit<NodeStats, "ageMs">; at: number } | null = null;
  private pointerSpot: { x: number; y: number; drawn: boolean } | null = null;
  /** The environment's latest cursor, and the images seen so far by id. */
  private cursor: ServerMessage | null = null;
  private readonly cursorImages = new Map<string, CursorImage>();
  private readonly audio: HTMLAudioElement;
  private audioTrack: MediaStreamTrack | null = null;
  private muted: boolean;
  transport: Transport | null = null;
  private wt: {
    worker: Worker;
    frames: WritableStreamDefaultWriter<VideoFrame>;
    audio: AudioDecoder;
    /** The stream being shown (the streamer starts at 0) and its decoder. */
    stream: number;
    pipeline: VideoPipeline;
    /** A codec switch asked for: its decoder, for the next stream's frames. */
    switching: { pipeline: VideoPipeline; done: (err?: Error) => void } | null;
    /** Frame id → server send time (µs, 32 bits) and when it reached the decoder. */
    sent: Map<number, { ts: number; decodeAt: number }>;
    decodeMs: number[];
    bytes: number;
    bytesAt: number;
    mbps: number | null;
    lost: number;
    recovered: number;
    shown: number[];
    /** Server send → decoded per frame, the last second's. */
    delivery: { at: number; ms: number }[];
    /** When frames arrived (the worker's clock), the last second's and one before. */
    arrivals: number[];
  } | null = null;

  constructor(private readonly options: PlayerOptions) {
    this.codec = options.codec ?? supportedCodecs()[0] ?? "h264";
    this.muted = options.muted ?? false;
    this.audio = document.createElement("audio");
    this.audio.muted = this.muted;
    this.audio.volume = clampVolume(options.volume ?? 1);
  }

  async connect(): Promise<void> {
    this.close();
    this.streamFps = null;
    this.setState("connecting");
    const want = isPyroWave(this.codec) ? "webtransport" : (this.options.transport ?? "auto");
    if (this.options.webTransport && want !== "webrtc" && supportsWebTransport()) {
      try {
        await this.connectWebTransport();
        return;
      } catch (err) {
        this.close();
        if (want === "webtransport") {
          this.setState("failed", err instanceof Error ? err.message : String(err));
          throw err;
        }
        // Fall back to WebRTC.
        this.setState("connecting");
      }
    }
    await this.connectWebRtc();
  }

  /** The frame rate the streamer encodes at, once it has said. */
  get fps(): number | null {
    return this.streamFps;
  }

  /**
   * Changes the frame rate (60, 90 or 120) for the environment's stream: the
   * streamer reconfigures its encoder in place and scales the bitrate, with
   * no gap in the picture. Resolves with the rate now; rejects if this page
   * doesn't have the controls, or the streamer refuses or doesn't answer in
   * 3 s. Not for apps whose display can't change refresh (`fixedSize`).
   */
  setFps(fps: FrameRate): Promise<number> {
    if (this.state !== "connected") return Promise.reject(new Error("not connected"));
    if (!this.hasControl) return Promise.reject(new Error("this page doesn't have the controls"));
    if (fps === this.streamFps) return Promise.resolve(fps);
    this.fpsChange?.fail(new Error("another change replaced it"));
    return new Promise<number>((resolve, reject) => {
      const timer = setTimeout(() => finish(() => reject(new Error("the streamer didn't answer"))), FPS_TIMEOUT_MS);
      const finish = (settle: () => void) => {
        clearTimeout(timer);
        if (this.fpsChange === change) this.fpsChange = null;
        settle();
      };
      const change = {
        done: (now: number) => finish(() => resolve(now)),
        fail: (err: Error) => finish(() => reject(err)),
      };
      this.fpsChange = change;
      this.send({ t: "fps", fps });
    });
  }

  /** Records the frame rate the streamer reports (hello, stats, or the answer to `setFps`). */
  private noteFps(fps: number | undefined): void {
    if (typeof fps !== "number" || !Number.isFinite(fps) || fps <= 0 || fps === this.streamFps) return;
    this.streamFps = fps;
    this.options.onFps?.(fps);
  }

  /**
   * Switches the video codec. Over WebTransport the session switches in
   * place: the picture stays up until the first frame in the new codec.
   * Otherwise (WebRTC, or a streamer that doesn't switch) it reconnects.
   */
  async switchCodec(codec: Codec): Promise<void> {
    if (codec === this.codec && this.state === "connected") return;
    const wt = this.wt;
    if (wt && this.state === "connected" && !wt.switching) {
      try {
        const pipeline = await this.newPipeline(wt, codec);
        if (this.wt !== wt) {
          this.closePipeline(pipeline);
          return;
        }
        await new Promise<void>((resolve, reject) => {
          const timer = setTimeout(() => {
            if (wt.switching?.pipeline !== pipeline) return;
            wt.switching = null;
            this.closePipeline(pipeline);
            reject(new Error("the streamer didn't switch"));
          }, SWITCH_TIMEOUT_MS);
          wt.switching = {
            pipeline,
            done: (err) => {
              clearTimeout(timer);
              if (err) reject(err);
              else resolve();
            },
          };
          this.send({ t: "codec", codec });
        });
        return;
      } catch {
        // Reconnect in the new codec instead.
      }
    }
    this.codec = codec;
    await this.connect();
  }

  private async connectWebRtc(): Promise<void> {
    const { video } = this.options;
    const pc = new RTCPeerConnection({ iceServers: this.options.iceServers ?? [] });
    this.pc = pc;
    // Render as soon as frames are decodable (the streamer also asks for it),
    // and keep NetEq's audio buffer at its minimum.
    for (const kind of ["video", "audio"]) {
      const transceiver = pc.addTransceiver(kind, { direction: "recvonly" });
      (transceiver.receiver as RTCRtpReceiver & { jitterBufferTarget?: number }).jitterBufferTarget = 0;
    }
    const control = pc.createDataChannel("control", { ordered: true });
    this.control = control;

    pc.ontrack = (e) => {
      if (e.track.kind === "audio") {
        this.audioTrack = e.track;
        this.audio.srcObject = new MediaStream([e.track]);
        void this.playAudio();
        return;
      }
      video.srcObject = new MediaStream([e.track]);
      video.muted = true;
      video.playsInline = true;
      void video.play().catch(() => {});
    };
    pc.onconnectionstatechange = () => {
      const s = pc.connectionState;
      if (s === "connected") {
        this.transport = "webrtc";
        this.options.onTransport?.("webrtc");
        this.setState("connected");
      }
      else if (s === "failed") this.setState("failed", "the connection to the node failed");
      else if (s === "disconnected" || s === "closed") this.setState("disconnected");
    };
    control.onopen = () => this.onControlOpen();
    control.onmessage = (e) => {
      for (const line of String(e.data).split("\n")) if (line) this.onServerMessage(line);
    };

    const offer = await pc.createOffer();
    try {
      await pc.setLocalDescription({ type: "offer", sdp: stereoOpus(offer.sdp ?? "") });
    } catch {
      await pc.setLocalDescription(offer); // munging refused: mono
    }
    try {
      const answer = await this.options.signal(pc.localDescription!.toJSON(), this.codec);
      if (this.pc !== pc) return; // closed meanwhile
      await pc.setRemoteDescription(answer);
    } catch (err) {
      this.setState("failed", err instanceof Error ? err.message : String(err));
      throw err;
    }
  }

  close(): void {
    this.cleanup.splice(0).forEach((f) => f());
    this.closeWebTransport();
    this.transport = null;
    this.probe?.stop();
    this.input?.dispose();
    this.input = null;
    this.pads?.stop();
    this.pads = null;
    this.options.onControllers?.([]);
    this.nodeStats = null;
    this.sentCounts = [];
    this.fpsChange?.fail(new Error("the session closed"));
    this.control?.close();
    this.pc?.close();
    this.pc = this.control = null;
    this.audio.srcObject = null;
    this.audioTrack = null;
    this.sentAt.clear();
    this.latencies.length = 0;
    this.rtcDelivery.length = 0;
    this.lastSize = "";
    if (this.state !== "idle") this.setState("idle");
  }

  /** Sound on or off. Turning it on from a click also satisfies autoplay. */
  setMuted(muted: boolean): void {
    this.muted = muted;
    this.audio.muted = muted;
    if (!muted) void this.playAudio();
  }

  /** Sound's level, 0..1, on this page only (the environment's own volume stays). */
  setVolume(volume: number): void {
    this.audio.volume = clampVolume(volume);
  }

  private async playAudio(): Promise<void> {
    if (this.muted || !this.audio.srcObject || !this.audio.paused) return;
    try {
      await this.audio.play();
      this.options.onAudioBlocked?.(false);
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotAllowedError") this.options.onAudioBlocked?.(true);
    }
  }

  /** Raw relative mouse (games). Esc releases it. */
  lockPointer(): Promise<void> {
    return this.input?.lockPointer() ?? Promise.resolve();
  }

  async readStats(): Promise<StatsSnapshot | null> {
    const latencyMs = percentile(this.latencies.slice(-(this.streamFps ?? 60)), 0.5);
    let snapshot: StatsSnapshot;
    if (this.wt) snapshot = this.webTransportStats(latencyMs);
    else if (this.pc) snapshot = await this.stats.read(this.pc, latencyMs);
    else return null;
    const ageMs = this.nodeStats ? performance.now() - this.nodeStats.at : Infinity;
    snapshot.targetFps = this.streamFps;
    snapshot.sentFps = this.sentFps();
    snapshot.node = this.nodeStats && ageMs <= NODE_STATS_FRESH_MS ? { ...this.nodeStats.stats, ageMs } : null;
    return snapshot;
  }

  /** Frames per second the streamer sent over its last few reports; null with under two, or none lately. */
  private sentFps(): number | null {
    const now = performance.now();
    const c = this.sentCounts.filter((s) => now - s.at <= SENT_WINDOW_MS);
    this.sentCounts = c;
    if (c.length < 2) return null;
    const first = c[0]!;
    const last = c[c.length - 1]!;
    const dt = last.at - first.at;
    return dt > 0 && last.frames >= first.frames ? ((last.frames - first.frames) * 1000) / dt : null;
  }

  /** Click → screen, `count` synthetic clicks (use with the test pattern). */
  async runProbe(count = 25): Promise<ProbeResult> {
    const probe = new ClickProbe(
      this.options.video,
      (m) => this.sendInput(m),
      (us) => this.toLocal(us),
      this.muted ? null : this.audioTrack,
    );
    this.probe = probe;
    try {
      return await probe.run(count);
    } finally {
      this.probe = null;
    }
  }

  private setState(state: PlayerState, detail?: string): void {
    this.state = state;
    this.options.onState?.(state, detail);
  }

  private send(msg: Record<string, unknown>): void {
    if (this.wt) this.wt.worker.postMessage({ type: "send", line: JSON.stringify(msg) } satisfies ToWorker);
    else if (this.control?.readyState === "open") this.control.send(JSON.stringify(msg) + "\n");
  }

  private sendInput(msg: Record<string, unknown>): void {
    if (this.hasControl) this.send({ t: "input", ...msg });
  }

  /** The controllers this page sends, once connected. */
  get controllers(): ControllerManager | null {
    return this.pads;
  }

  /**
   * Opens the browser's picker for a controller WebHID can reach (one the
   * Gamepad API can't see, such as a Steam Controller). Call it from a click.
   * Resolves with how many controllers it added.
   */
  async connectHidController(): Promise<number> {
    if (!this.pads) throw new Error("Not connected yet.");
    return this.pads.connectHid();
  }

  /** Asks for the controls (owners and admins get them). */
  takeControl(): void {
    this.send({ t: "take_control" });
  }

  private onControlOpen(): void {
    const { video } = this.options;
    this.input = new InputCapture(video, (m) => this.sendInput(m), {
      onPaste: (text) => this.send({ t: "clipboard", text }),
    });
    this.pads?.stop();
    const pads = new ControllerManager({ send: (m) => this.sendInput(m) });
    pads.onChange((list) => this.options.onControllers?.(list));
    pads.start();
    this.pads = pads;
    video.focus();
    // Desktop mode draws the cursor here, with no stream delay; a locked
    // pointer (games) leaves it to the picture.
    const cursorMode = () => {
      if (this.hasControl) this.send({ t: "cursor", client: !this.input?.locked });
      this.applyCursor();
      this.placePointer();
    };
    cursorMode();
    document.addEventListener("pointerlockchange", cursorMode);
    window.addEventListener("resize", cursorMode);
    this.cleanup.push(() => {
      document.removeEventListener("pointerlockchange", cursorMode);
      window.removeEventListener("resize", cursorMode);
    });
    // Any click or key in the picture is the gesture autoplay waits for.
    const unblock = () => void this.playAudio();
    video.addEventListener("pointerdown", unblock);
    video.addEventListener("keydown", unblock);
    this.cleanup.push(() => {
      video.removeEventListener("pointerdown", unblock);
      video.removeEventListener("keydown", unblock);
    });

    // Clock sync: the lowest-RTT ping gives the best offset.
    const ping = () => this.send({ t: "ping", c: performance.timeOrigin + performance.now() });
    ping();
    const pinger = setInterval(ping, 1000);
    this.cleanup.push(() => clearInterval(pinger));
    if (!this.wt) this.reportRates();

    // The picture follows the element's size (in device pixels, capped).
    const observer = new ResizeObserver(() => this.requestSize());
    observer.observe(video);
    this.cleanup.push(() => observer.disconnect());
    this.requestSize();

    // Every presented frame: latency from the server's send time, and the probe.
    let watching = true;
    const onFrame: VideoFrameRequestCallback = (_now, md) => {
      if (!watching) return;
      this.onFrame(md);
      video.requestVideoFrameCallback(onFrame);
    };
    video.requestVideoFrameCallback(onFrame);
    this.cleanup.push(() => (watching = false));
  }

  /**
   * WebRTC: every 100 ms, the report the WebTransport worker sends for the
   * streamer's rate control (plan §3.1 rule 1): send → complete (from
   * presented frames, the last 150 ms), what arrived (200 ms) and the share
   * of packets lost (1 s). The delay is the lower quartile, not the median:
   * a frame that needed a retransmission completes a round trip late, and
   * at a few percent loss most frames do, while a queue delays them all. A page that presents no frames
   * (hidden) reports no delay, and the streamer holds its rate.
   *
   * Also the bound on a resync (plan §3.1 rule 4): when frames stop
   * decoding for 300 ms while data still arrives, Chrome is waiting on
   * retransmissions a congested path may not deliver for seconds; a
   * keyframe is asked for (at most once a second).
   */
  private reportRates(): void {
    const arrived: { at: number; bytes: number }[] = [];
    const counts: { at: number; received: number; lost: number }[] = [];
    let decoded = { frames: 0, at: performance.now() };
    let askedAt = 0;
    let busy = false;
    const report = async () => {
      const pc = this.pc;
      if (!pc || busy) return;
      busy = true;
      let video: RTCInboundRtpStreamStats | undefined;
      try {
        (await pc.getStats()).forEach((s: RTCStats) => {
          if (s.type === "inbound-rtp" && (s as RTCInboundRtpStreamStats).kind === "video") video = s as RTCInboundRtpStreamStats;
        });
      } finally {
        busy = false;
      }
      if (!video) return;
      const t = performance.now();
      arrived.push({ at: t, bytes: video.bytesReceived ?? 0 });
      counts.push({ at: t, received: video.packetsReceived ?? 0, lost: video.packetsLost ?? 0 });
      while (arrived.length > 2 && t - arrived[0]!.at > 250) arrived.shift();
      while (counts.length > 2 && t - counts[0]!.at > 1000) counts.shift();
      const first = arrived[0]!;
      const mbps = t > first.at ? ((arrived[arrived.length - 1]!.bytes - first.bytes) * 8) / ((t - first.at) * 1000) : 0;
      if ((video.framesDecoded ?? 0) !== decoded.frames) decoded = { frames: video.framesDecoded ?? 0, at: t };
      else if (t - decoded.at > 300 && mbps > 0.2 && t - askedAt > 1000) {
        askedAt = t;
        this.send({ t: "keyframe" });
      }
      const [a, b] = [counts[0]!, counts[counts.length - 1]!];
      const lost = Math.max(0, b.lost - a.lost);
      const expected = b.received - a.received + lost;
      const now = performance.timeOrigin + t;
      while (this.rtcDelivery.length && now - this.rtcDelivery[0]!.at > 150) this.rtcDelivery.shift();
      const delays = this.rtcDelivery.map((d) => d.ms).sort((x, y) => x - y);
      const d = delays.length ? delays[Math.floor(delays.length / 4)]! : null;
      this.send({
        t: "report",
        r: Math.round(mbps * 100) / 100,
        ...(d !== null && { d: Math.round(d * 10) / 10 }),
        ...(expected > 0 && { l: Math.round((lost / expected) * 10_000) / 10_000 }),
      });
    };
    const reporter = setInterval(() => void report(), 100);
    this.cleanup.push(() => clearInterval(reporter));
  }

  private resizeTimer?: ReturnType<typeof setTimeout>;

  /** Asks for a picture the element's size, after resizing settles. */
  private requestSize(): void {
    clearTimeout(this.resizeTimer);
    if (this.options.fixedSize) return;
    // The controller's page sizes the picture; viewers scale it.
    if (!this.hasControl) return;
    this.resizeTimer = setTimeout(() => {
      const r = this.options.video.getBoundingClientRect();
      const max = this.options.maxSize ?? { width: 2560, height: 1440 };
      let w = r.width * devicePixelRatio;
      let h = r.height * devicePixelRatio;
      const scale = Math.min(1, max.width / w, max.height / h);
      w = Math.floor((w * scale) / 8) * 8;
      h = Math.floor((h * scale) / 8) * 8;
      const size = `${w}x${h}`;
      if (w < 320 || h < 240 || size === this.lastSize) return;
      this.lastSize = size;
      this.send({ t: "resize", w, h });
    }, 250);
  }

  private onServerMessage(line: string): void {
    let msg: ServerMessage;
    try {
      msg = JSON.parse(line) as ServerMessage;
    } catch {
      return;
    }
    switch (msg.t) {
      case "pong": {
        const now = performance.timeOrigin + performance.now();
        const rtt = now - msg.c!;
        if (!this.offset || rtt < this.offset.rtt) {
          this.offset = { rtt, ms: msg.c! + rtt / 2 - msg.s_us! / 1000 };
          this.wt?.worker.postMessage({ type: "clock", offsetMs: this.offset.ms } satisfies ToWorker);
        }
        break;
      }
      case "sent":
        this.sentAt.set(msg.rtp!, msg.s_us!);
        if (this.sentAt.size > 600) {
          const oldest = this.sentAt.keys().next().value;
          if (oldest !== undefined) this.sentAt.delete(oldest);
        }
        break;
      case "probe":
        this.probe?.acknowledged(msg.id as number, msg.s_us!);
        break;
      case "clipboard":
        if (typeof msg.text === "string") void this.copied(msg.text);
        break;
      case "cursor":
        this.cursor = msg;
        this.applyCursor();
        this.placePointer();
        break;
      case "floor": {
        const gained = !!msg.control && !this.hasControl;
        this.hasControl = !!msg.control;
        if (gained) {
          // Our turn: our size and cursor mode.
          this.lastSize = "";
          this.requestSize();
          this.send({ t: "cursor", client: !this.input?.locked });
          // Pads were sent nothing while the controls were elsewhere.
          this.pads?.resync();
        }
        this.applyCursor();
        this.placePointer();
        this.options.onFloor?.(this.hasControl, msg.viewers ?? 1);
        break;
      }
      case "system": {
        const stats = toNodeStats(msg as unknown as Record<string, unknown>);
        this.nodeStats = stats ? { stats, at: performance.now() } : null;
        break;
      }
      case "status":
        this.options.onStatus?.(toStatus(msg));
        break;
      // The app rumbles pad `i`: play it on that physical controller.
      case "rumble":
        if (typeof msg.i === "number") this.pads?.rumble(msg.i, msg.lo ?? 0, msg.hi ?? 0, msg.ms ?? 0);
        break;
      // The rest of what an app does to a pad (docs/controllers.md): played where the controller can.
      case "haptic":
        if (typeof msg.i === "number") {
          this.pads?.haptic(msg.i, msg.side === "right" ? "right" : "left", msg.amp ?? 0, msg.on_us ?? 0, msg.off_us ?? 0, msg.count ?? 1);
        }
        break;
      case "led":
        if (typeof msg.i === "number") this.pads?.led(msg.i, msg.r ?? 0, msg.g ?? 0, msg.b ?? 0);
        break;
      case "players":
        if (typeof msg.i === "number") this.pads?.players(msg.i, msg.mask ?? 0);
        break;
      case "trigger":
        if (typeof msg.i === "number" && Array.isArray(msg.effect)) {
          this.pads?.trigger(msg.i, msg.side === "right" ? "right" : "left", msg.effect.map(Number));
        }
        break;
      case "pointer":
        this.pointerSpot = { x: msg.x ?? 0, y: msg.y ?? 0, drawn: !!msg.drawn };
        this.placePointer();
        break;
      case "hello":
        this.noteFps((msg.stream as unknown as { fps?: number } | undefined)?.fps);
        break;
      case "stats":
        this.noteFps(msg.fps);
        if (typeof msg.frames_sent === "number") {
          this.sentCounts.push({ at: performance.now(), frames: msg.frames_sent });
          if (this.sentCounts.length > 8) this.sentCounts.shift();
        }
        break;
      case "fps": {
        // The answer to `setFps`: the rate now, with an `error` if it didn't change.
        this.noteFps(msg.fps);
        const change = this.fpsChange;
        if (change) {
          if (msg.error) change.fail(new Error(msg.error));
          else change.done(msg.fps ?? this.streamFps ?? 0);
        }
        break;
      }
      case "codec": {
        // The answer to a switch: it completes with the new stream's first
        // frame, unless it failed or nothing changed.
        const wt = this.wt;
        const switching = wt?.switching;
        if (!wt || !switching) break;
        if (msg.error || msg.stream === wt.stream) {
          wt.switching = null;
          this.closePipeline(switching.pipeline);
          switching.done(msg.error ? new Error(msg.error) : undefined);
        }
        break;
      }
    }
  }

  /** The environment's cursor as the element's CSS cursor, at its on-screen size. */
  private applyCursor(): void {
    const msg = this.cursor;
    const { video } = this.options;
    if (!this.hasControl) {
      // A viewer's own mouse is its own; the controller's pointer is drawn over the picture.
      video.style.cursor = "";
      return;
    }
    if (!msg || this.input?.locked) return;
    if (msg.kind === "hidden") {
      video.style.cursor = "none";
    } else if (msg.kind === "named") {
      video.style.cursor = msg.name && CSS.supports("cursor", msg.name) ? msg.name : "default";
    } else {
      const image = this.cursorImage(msg);
      if (!image) {
        video.style.cursor = "default";
        return;
      }
      // Stream pixels per CSS pixel, so the cursor is as big as in the picture.
      const r = video.getBoundingClientRect();
      const k = 1 / Math.min(r.width / (video.videoWidth || r.width), r.height / (video.videoHeight || r.height));
      const scaled = `image-set(url("${image.url}") ${k.toFixed(3)}x) ${Math.round(image.x / k)} ${Math.round(image.y / k)}, default`;
      video.style.cursor = CSS.supports("cursor", scaled) ? scaled : `url("${image.url}") ${image.x} ${image.y}, default`;
    }
  }

  /**
   * A viewer's page draws the controller's pointer over the picture when the
   * picture doesn't show it (the controller's page draws its own cursor).
   */
  private placePointer(): void {
    const { video } = this.options;
    const spot = this.pointerSpot;
    const show = !this.hasControl && spot && !spot.drawn && this.cursor?.kind !== "hidden";
    if (!show) {
      if (this.pointerEl) this.pointerEl.style.display = "none";
      return;
    }
    const parent = video.offsetParent as HTMLElement | null;
    if (!parent) return;
    if (!this.pointerEl) {
      const el = document.createElement("div");
      el.style.cssText = "position:absolute;pointer-events:none;z-index:5;line-height:0;";
      parent.appendChild(el);
      this.pointerEl = el;
      this.cleanup.push(() => el.remove());
    }
    const el = this.pointerEl;
    // Where the picture sits in the element (object-fit: contain).
    const r = video.getBoundingClientRect();
    const p = parent.getBoundingClientRect();
    const vw = video.videoWidth || r.width;
    const vh = video.videoHeight || r.height;
    const scale = Math.min(r.width / vw, r.height / vh);
    const left = r.left - p.left + (r.width - vw * scale) / 2 + spot.x * vw * scale;
    const top = r.top - p.top + (r.height - vh * scale) / 2 + spot.y * vh * scale;
    const image = this.cursor?.kind === "image" ? this.cursorImage(this.cursor) : null;
    const key = image ? image.url : (this.cursor?.name ?? "default");
    if (el.dataset.key !== key) {
      el.dataset.key = key;
      if (image) {
        el.innerHTML = "";
        const img = document.createElement("img");
        img.src = image.url;
        el.appendChild(img);
      } else {
        el.innerHTML = this.cursor?.name === "text" ? I_BEAM : ARROW;
      }
    }
    const img = el.querySelector("img");
    if (img && this.cursor?.w && this.cursor?.h) {
      img.style.width = `${this.cursor.w * scale}px`;
      img.style.height = `${this.cursor.h * scale}px`;
    }
    const hx = image ? image.x * scale : this.cursor?.name === "text" ? 6 : 1;
    const hy = image ? image.y * scale : this.cursor?.name === "text" ? 9 : 1;
    el.style.display = "block";
    el.style.transform = `translate(${left - hx}px, ${top - hy}px)`;
    el.style.left = "0";
    el.style.top = "0";
  }

  private cursorImage(msg: ServerMessage): CursorImage | null {
    const id = String(msg.id);
    const cached = this.cursorImages.get(id);
    if (cached) return cached;
    if (!msg.rgba || !msg.w || !msg.h) return null;
    const pixels = Uint8ClampedArray.from(atob(msg.rgba), (c) => c.charCodeAt(0));
    if (pixels.length !== msg.w * msg.h * 4) return null;
    const canvas = document.createElement("canvas");
    canvas.width = msg.w;
    canvas.height = msg.h;
    canvas.getContext("2d")?.putImageData(new ImageData(pixels, msg.w, msg.h), 0, 0);
    const image = { url: canvas.toDataURL("image/png"), x: msg.x ?? 0, y: msg.y ?? 0 };
    if (this.cursorImages.size >= 64) this.cursorImages.delete(this.cursorImages.keys().next().value!);
    this.cursorImages.set(id, image);
    return image;
  }

  /** Text an app copied: onto this device's clipboard, now or on the next click. */
  private async copied(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.options.onClipboard?.(text, true);
    } catch {
      // Not focused, or the browser wants a gesture: retry on the next one.
      this.options.onClipboard?.(text, false);
      const retry = () => void navigator.clipboard.writeText(text).catch(() => {});
      this.options.video.addEventListener("pointerdown", retry, { once: true });
    }
  }

  private toLocal(serverUs: number): number | null {
    return this.offset ? serverUs / 1000 + this.offset.ms : null;
  }

  /**
   * A 32-bit server timestamp (µs, wraps every ~71 min) near the server's
   * now: just behind it, or just ahead (the clock estimate assumes symmetric
   * paths, so a stamp can look a little early).
   */
  private unwrap(ts: number): number {
    if (!this.offset) return ts;
    const nowUs = (performance.timeOrigin + performance.now() - this.offset.ms) * 1000;
    const span = 2 ** 32;
    const behind = ((nowUs % span) - ts + span) % span;
    return behind > span / 2 ? nowUs + (span - behind) : nowUs - behind;
  }

  /**
   * The server time (µs) of a frame's RTP timestamp: the streamer stamps
   * each frame's encode time on the session clock at 90 kHz, wrapping every
   * ~13 h; the nearest wrap to the server's now.
   */
  private unwrapRtp(rtp: number): number {
    const us = (rtp * 100) / 9;
    if (!this.offset) return us;
    const nowUs = (performance.timeOrigin + performance.now() - this.offset.ms) * 1000;
    const span = (2 ** 32 * 100) / 9;
    return us + Math.round((nowUs - us) / span) * span;
  }

  private async connectWebTransport(): Promise<void> {
    const offer = await this.options.webTransport!(this.codec);
    const { video } = this.options;
    const worker = new Worker(new URL("./wt-worker.ts", import.meta.url), { type: "module" });
    const frames = new MediaStreamTrackGenerator<VideoFrame>({ kind: "video" });
    const sound = new MediaStreamTrackGenerator<AudioData>({ kind: "audio" });
    const soundWriter = sound.writable.getWriter();
    const wt: NonNullable<Player["wt"]> = {
      worker,
      frames: frames.writable.getWriter(),
      audio: new AudioDecoder({
        output: (data) => void soundWriter.write(data),
        error: () => undefined,
      }),
      stream: 0,
      pipeline: null as unknown as VideoPipeline,
      switching: null,
      sent: new Map(),
      decodeMs: [],
      bytes: 0,
      bytesAt: performance.now(),
      mbps: null,
      lost: 0,
      recovered: 0,
      shown: [],
      delivery: [],
      arrivals: [],
    };
    try {
      // PyroWave decodes on WebGPU; set it up first, it can fail.
      wt.pipeline = await this.newPipeline(wt, this.codec);
    } catch (err) {
      worker.terminate();
      throw err;
    }
    wt.audio.configure({ codec: "opus", sampleRate: 48_000, numberOfChannels: 2 });
    this.wt = wt;
    video.srcObject = new MediaStream([frames]);
    video.muted = true;
    video.playsInline = true;
    void video.play().catch(() => {});
    this.audioTrack = sound;
    this.audio.srcObject = new MediaStream([sound]);
    void this.playAudio();

    const ready = new Promise<void>((resolve, reject) => {
      worker.onmessage = (e: MessageEvent<FromWorker>) => {
        const msg = e.data;
        switch (msg.type) {
          case "ready": {
            this.transport = "webtransport";
            this.options.onTransport?.("webtransport");
            this.onControlOpen();
            this.setState("connected");
            resolve();
            break;
          }
          case "line":
            this.onServerMessage(msg.line);
            break;
          case "video":
            if (msg.stream !== wt.stream) {
              // The first frame in the codec switched to; stragglers of an
              // older stream (the worker drops most) are ignored.
              if (!wt.switching || !newerStream(wt.stream, msg.stream)) break;
              this.adoptStream(wt, msg.stream);
            }
            this.decodeVideo(wt, msg);
            break;
          case "audio":
            if (wt.audio.state === "configured") {
              wt.audio.decode(new EncodedAudioChunk({ type: "key", timestamp: msg.id * 10_000, data: msg.data }));
            }
            break;
          case "lost":
            wt.lost += msg.frames;
            break;
          case "recovered":
            wt.recovered += msg.frames;
            break;
          case "bytes": {
            const t = performance.now();
            wt.mbps = (msg.bytes * 8) / ((t - wt.bytesAt) * 1000);
            wt.bytesAt = t;
            break;
          }
          case "closed":
            if (this.wt !== wt) break;
            if (this.state === "connected") this.setState("disconnected", msg.reason);
            reject(new Error(msg.reason));
            break;
        }
      };
    });
    worker.postMessage({ type: "start", urls: offer.urls, certHash: offer.certHash } satisfies ToWorker);
    if (this.offset) worker.postMessage({ type: "clock", offsetMs: this.offset.ms } satisfies ToWorker);
    await ready;
  }

  /** A decoder for `codec`'s frames, writing into the session's video track. */
  private async newPipeline(wt: NonNullable<Player["wt"]>, codec: Codec): Promise<VideoPipeline> {
    const p: VideoPipeline = { codec, pyro: null, video: null, sawKey: false, closed: false };
    if (isPyroWave(codec)) p.pyro = await PyroPresenter.create();
    else p.video = this.newVideoDecoder(wt, p);
    return p;
  }

  private newVideoDecoder(wt: NonNullable<Player["wt"]>, p: VideoPipeline): VideoDecoder {
    const decoder = new VideoDecoder({
      output: (frame) => {
        const sent = wt.sent.get(frame.timestamp);
        if (sent) {
          this.pushDecodeMs(wt, performance.now() - sent.decodeAt);
          this.noteDelivery(wt, sent.ts);
        }
        void wt.frames.write(frame); // the sink closes it
      },
      error: () => {
        // An error closes the decoder: a new one, from the next keyframe.
        if (p.closed || this.wt !== wt) return;
        p.sawKey = false;
        this.send({ t: "keyframe" });
        p.video = this.newVideoDecoder(wt, p);
      },
    });
    decoder.configure(this.decoderConfig(p.codec as HwCodec));
    return decoder;
  }

  private closePipeline(p: VideoPipeline): void {
    if (p.closed) return;
    p.closed = true;
    if (p.video && p.video.state !== "closed") p.video.close();
    // Frames it made may still be on their way to the screen.
    if (p.pyro) setTimeout(() => p.pyro!.destroy(), 1000);
  }

  /** The switch's stream has started: decode it from now on. */
  private adoptStream(wt: NonNullable<Player["wt"]>, stream: number): void {
    const switching = wt.switching!;
    wt.switching = null;
    this.closePipeline(wt.pipeline);
    wt.pipeline = switching.pipeline;
    wt.stream = stream;
    this.codec = switching.pipeline.codec;
    switching.done();
  }

  private decodeVideo(wt: NonNullable<Player["wt"]>, msg: Extract<FromWorker, { type: "video" }>): void {
    wt.arrivals.push(msg.lastAt);
    while (wt.arrivals.length > 2 && msg.lastAt - wt.arrivals[1]! > 1000) wt.arrivals.shift();
    const p = wt.pipeline;
    if (p.pyro) {
      const started = performance.now();
      this.noteSent(wt, msg.id, msg.sendTs, started);
      const frame = p.pyro.decode(new Uint8Array(msg.data), msg.id, msg.partial ?? false);
      if (frame) {
        this.pushDecodeMs(wt, performance.now() - started);
        this.noteDelivery(wt, msg.sendTs);
        void wt.frames.write(frame);
      }
      return;
    }
    if (!p.sawKey && !msg.key) return;
    p.sawKey = true;
    if (!p.video || p.video.state !== "configured") return;
    this.noteSent(wt, msg.id, msg.sendTs, performance.now());
    p.video.decode(new EncodedVideoChunk({ type: msg.key ? "key" : "delta", timestamp: msg.id, data: msg.data }));
  }

  private noteSent(wt: NonNullable<Player["wt"]>, id: number, ts: number, decodeAt: number): void {
    wt.sent.set(id, { ts, decodeAt });
    if (wt.sent.size > 600) wt.sent.delete(wt.sent.keys().next().value!);
  }

  /** A frame sent at server time `ts` (µs, 32 bits) is decoded now. */
  private noteDelivery(wt: NonNullable<Player["wt"]>, ts: number): void {
    const sentAt = this.toLocal(this.unwrap(ts));
    if (sentAt === null) return;
    const now = performance.timeOrigin + performance.now();
    wt.delivery.push({ at: now, ms: now - sentAt });
    while (wt.delivery.length && now - wt.delivery[0]!.at > 1000) wt.delivery.shift();
  }

  private pushDecodeMs(wt: NonNullable<Player["wt"]>, ms: number): void {
    wt.decodeMs.push(ms);
    if (wt.decodeMs.length > 120) wt.decodeMs.splice(0, wt.decodeMs.length - 120);
  }

  private decoderConfig(codec: HwCodec): VideoDecoderConfig {
    return { codec: WEBCODECS[codec], hardwareAcceleration: "prefer-hardware", optimizeForLatency: true };
  }

  private closeWebTransport(): void {
    const wt = this.wt;
    if (!wt) return;
    this.wt = null;
    wt.worker.postMessage({ type: "close" } satisfies ToWorker);
    setTimeout(() => wt.worker.terminate(), 500);
    if (wt.audio.state !== "closed") wt.audio.close();
    this.closePipeline(wt.pipeline);
    if (wt.switching) {
      this.closePipeline(wt.switching.pipeline);
      wt.switching.done(new Error("the session closed"));
      wt.switching = null;
    }
  }

  private frameGap(wt: NonNullable<Player["wt"]>): number | null {
    const now = performance.timeOrigin + performance.now();
    const a = wt.arrivals;
    if (!a.length) return null;
    let gap = now - a[a.length - 1]!;
    for (let i = 1; i < a.length; i++) if (now - a[i]! <= 1000) gap = Math.max(gap, a[i]! - a[i - 1]!);
    return gap;
  }

  private deliveryStats(wt: NonNullable<Player["wt"]>): { deliveryMs: number | null; deliveryP95Ms: number | null } {
    const now = performance.timeOrigin + performance.now();
    const ms = wt.delivery
      .filter((d) => now - d.at <= 1000)
      .map((d) => d.ms)
      .sort((a, b) => a - b);
    return { deliveryMs: percentile(ms, 0.5), deliveryP95Ms: percentile(ms, 0.95) };
  }

  private webTransportStats(latencyMs: number | null): StatsSnapshot {
    const wt = this.wt!;
    const t = performance.now();
    wt.shown = wt.shown.filter((at) => t - at < 1000);
    const { video } = this.options;
    const quality = video.getVideoPlaybackQuality?.();
    const decode = wt.decodeMs.length ? wt.decodeMs.reduce((a, b) => a + b, 0) / wt.decodeMs.length : null;
    return {
      codec: `${this.codec.toUpperCase()} · WebTransport`,
      width: video.videoWidth || null,
      height: video.videoHeight || null,
      fps: wt.shown.length,
      targetFps: null,
      sentFps: null,
      mbps: wt.mbps,
      decodeMs: decode,
      jitterMs: null,
      rttMs: this.offset?.rtt ?? null,
      packetsLost: wt.lost,
      framesRecovered: wt.recovered,
      framesDropped: quality?.droppedVideoFrames ?? 0,
      latencyMs,
      ...this.deliveryStats(wt),
      frameGapMs: this.frameGap(wt),
      node: null,
      audioJitterMs: null,
    };
  }

  private onFrame(md: VideoFrameCallbackMetadata): void {
    this.probe?.onFrame(md);
    let at: number | null = null;
    if (this.wt) {
      // The frame's timestamp is its id, in µs units.
      const sent = this.wt.sent.get(Math.round(md.mediaTime * 1e6));
      at = sent === undefined ? null : this.toLocal(this.unwrap(sent.ts));
      this.wt.shown.push(performance.now());
    } else {
      const sent = md.rtpTimestamp === undefined ? undefined : this.sentAt.get(md.rtpTimestamp);
      at = sent === undefined ? null : this.toLocal(sent);
      // For rate control, the send time from the RTP timestamp itself: the
      // `sent` messages share the congested path on the DataChannel, whose
      // retransmissions hold them back just when the delay matters.
      const encoded = md.rtpTimestamp === undefined ? null : this.toLocal(this.unwrapRtp(md.rtpTimestamp));
      if (encoded !== null && md.receiveTime !== undefined) {
        const now = performance.timeOrigin + performance.now();
        this.rtcDelivery.push({ at: now, ms: performance.timeOrigin + md.receiveTime - encoded });
        if (this.rtcDelivery.length > 60) this.rtcDelivery.splice(0, this.rtcDelivery.length - 60);
      }
    }
    if (at !== null) {
      this.latencies.push(performance.timeOrigin + md.presentationTime - at);
      if (this.latencies.length > 240) this.latencies.splice(0, this.latencies.length - 240);
    }
  }
}

/**
 * Asks for stereo Opus. Chrome decodes Opus as mono unless its own (local)
 * description says `stereo=1`; the streamer sends stereo.
 */
/** The controller's pointer on a viewer's page, for named cursors. */
const ARROW =
  '<svg width="14" height="21" viewBox="0 0 14 21"><path d="M1 1v16l4-4 3 7 2.5-1-3-7h5.5z" fill="#fff" stroke="#000" stroke-width="1.2" stroke-linejoin="round"/></svg>';
const I_BEAM =
  '<svg width="12" height="18" viewBox="0 0 12 18"><path d="M3 1h6M6 1v16M3 17h6" stroke="#fff" stroke-width="3"/><path d="M3 1h6M6 1v16M3 17h6" stroke="#000" stroke-width="1.2"/></svg>';

/** Stream b came after stream a (the streamer's 8-bit, wrapping counter). */
function newerStream(a: number, b: number): boolean {
  const d = (b - a) & 0xff;
  return d > 0 && d < 0x80;
}

export function stereoOpus(sdp: string): string {
  const pt = /a=rtpmap:(\d+) opus\/48000\/2/i.exec(sdp)?.[1];
  if (!pt) return sdp;
  return sdp.replace(new RegExp(`a=fmtp:${pt} ([^\\r\\n]*)`), (line, params: string) =>
    /(^|;)\s*stereo=/.test(params) ? line : `${line};stereo=1`,
  );
}

function clampVolume(v: number): number {
  return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 1;
}
