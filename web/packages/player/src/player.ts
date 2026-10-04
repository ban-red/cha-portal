// One connection to an environment's streamer, over one of two transports:
// - WebTransport (`cha-stream/1`, the Chromium fast path): video and audio
//   datagrams reassembled in a worker, decoded with WebCodecs and shown through
//   track generators (S1d: the fastest way to the screen);
// - WebRTC everywhere else: recvonly video (playout-delay 0) and stereo Opus.
// Video and audio are separate MediaStreams, so video never waits for lip
// sync. The control channel (a DataChannel, or WebTransport's first stream)
// carries input, resize requests and clock pings up, and per-frame send times
// down. The portal brokers the session; media flows straight from the node.

import { GamepadCapture } from "./gamepad";
import { InputCapture } from "./input";
import { ClickProbe, percentile, type ProbeResult } from "./probe";
import { StatsReader, type StatsSnapshot } from "./stats";
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
  onState?: (state: PlayerState, detail?: string) => void;
  /** Start with the sound off. */
  muted?: boolean;
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

export class Player {
  state: PlayerState = "idle";
  /** The video codec; `switchCodec()` changes it. */
  codec: Codec;
  private pc: RTCPeerConnection | null = null;
  private control: RTCDataChannel | null = null;
  private input: InputCapture | null = null;
  private gamepads: GamepadCapture | null = null;
  private probe: ClickProbe | null = null;
  private readonly stats = new StatsReader();
  private readonly cleanup: (() => void)[] = [];
  /** Local clock − server clock (ms), from the lowest-RTT ping so far. */
  private offset: { rtt: number; ms: number } | null = null;
  /** RTP timestamp → when the server sent that frame (server µs). */
  private readonly sentAt = new Map<number, number>();
  /** Server send → presented here (ms), recent frames. */
  private readonly latencies: number[] = [];
  private lastSize = "";
  /** This page has the controls (streamers before P2.6 don't say: assume so). */
  private hasControl = true;
  /** Where a viewer's page draws the controller's pointer. */
  private pointerEl: HTMLElement | null = null;
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
    shown: number[];
  } | null = null;

  constructor(private readonly options: PlayerOptions) {
    this.codec = options.codec ?? supportedCodecs()[0] ?? "h264";
    this.muted = options.muted ?? false;
    this.audio = document.createElement("audio");
    this.audio.muted = this.muted;
  }

  async connect(): Promise<void> {
    this.close();
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
    this.gamepads?.dispose();
    this.gamepads = null;
    this.control?.close();
    this.pc?.close();
    this.pc = this.control = null;
    this.audio.srcObject = null;
    this.audioTrack = null;
    this.sentAt.clear();
    this.latencies.length = 0;
    this.lastSize = "";
    if (this.state !== "idle") this.setState("idle");
  }

  /** Sound on or off. Turning it on from a click also satisfies autoplay. */
  setMuted(muted: boolean): void {
    this.muted = muted;
    this.audio.muted = muted;
    if (!muted) void this.playAudio();
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
    const latencyMs = percentile(this.latencies.slice(-60), 0.5);
    if (this.wt) return this.webTransportStats(latencyMs);
    if (!this.pc) return null;
    return this.stats.read(this.pc, latencyMs);
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

  /** Asks for the controls (owners and admins get them). */
  takeControl(): void {
    this.send({ t: "take_control" });
  }

  private onControlOpen(): void {
    const { video } = this.options;
    this.input = new InputCapture(video, (m) => this.sendInput(m), {
      onPaste: (text) => this.send({ t: "clipboard", text }),
    });
    this.gamepads = new GamepadCapture((m) => this.sendInput(m));
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

  private resizeTimer?: ReturnType<typeof setTimeout>;

  /** Asks for a picture the element's size, after resizing settles. */
  private requestSize(): void {
    clearTimeout(this.resizeTimer);
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
        }
        this.applyCursor();
        this.placePointer();
        this.options.onFloor?.(this.hasControl, msg.viewers ?? 1);
        break;
      }
      case "pointer":
        this.pointerSpot = { x: msg.x ?? 0, y: msg.y ?? 0, drawn: !!msg.drawn };
        this.placePointer();
        break;
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

  /** A 32-bit server timestamp (µs, wraps every ~71 min) near the server's now. */
  private unwrap(ts: number): number {
    if (!this.offset) return ts;
    const nowUs = (performance.timeOrigin + performance.now() - this.offset.ms) * 1000;
    const span = 2 ** 32;
    return nowUs - (((nowUs % span) - ts + span) % span);
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
      shown: [],
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
          case "ready":
            this.transport = "webtransport";
            this.options.onTransport?.("webtransport");
            this.onControlOpen();
            this.setState("connected");
            resolve();
            break;
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
        if (sent) this.pushDecodeMs(wt, performance.now() - sent.decodeAt);
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
    const p = wt.pipeline;
    if (p.pyro) {
      const started = performance.now();
      this.noteSent(wt, msg.id, msg.sendTs, started);
      const frame = p.pyro.decode(new Uint8Array(msg.data), msg.id, msg.partial ?? false);
      if (frame) {
        this.pushDecodeMs(wt, performance.now() - started);
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
      mbps: wt.mbps,
      decodeMs: decode,
      jitterMs: null,
      rttMs: this.offset?.rtt ?? null,
      packetsLost: wt.lost,
      framesDropped: quality?.droppedVideoFrames ?? 0,
      latencyMs,
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
