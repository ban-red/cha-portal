// One connection to an environment's streamer: recvonly WebRTC video
// (playout-delay 0, set by the streamer) and stereo Opus audio, each its own
// MediaStream so the browser never delays video for lip sync, and a `control`
// DataChannel carrying input, resize requests and clock pings up, and
// per-frame send times down. The portal brokers the offer/answer; media flows
// straight from the node.

import { GamepadCapture } from "./gamepad";
import { InputCapture } from "./input";
import { ClickProbe, percentile, type ProbeResult } from "./probe";
import { StatsReader, type StatsSnapshot } from "./stats";

export type Codec = "hevc" | "h264" | "av1";
export type PlayerState = "idle" | "connecting" | "connected" | "disconnected" | "failed";

const MIME: Record<Codec, string> = { hevc: "video/h265", h264: "video/h264", av1: "video/av1" };
/** Fastest to the screen first, as measured on the baseline (P1.3): HEVC
 *  decodes ~0.6 ms faster than H.264 in Chrome on Apple silicon, AV1 ~1.9 ms
 *  slower. */
const PREFERENCE: Codec[] = ["hevc", "h264", "av1"];

/** The codecs this browser can receive over WebRTC, best first. */
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
  /** The largest picture to ask for (default 2560×1440, the baseline). */
  maxSize?: { width: number; height: number };
  onState?: (state: PlayerState, detail?: string) => void;
  /** Start with the sound off. */
  muted?: boolean;
  /** Sound is waiting for a click or key press (the browser's autoplay rule). */
  onAudioBlocked?: (blocked: boolean) => void;
}

interface ServerMessage {
  t: string;
  id?: number;
  rtp?: number;
  s_us?: number;
  c?: number;
}

export class Player {
  state: PlayerState = "idle";
  readonly codec: Codec;
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
  private readonly audio: HTMLAudioElement;
  private audioTrack: MediaStreamTrack | null = null;
  private muted: boolean;

  constructor(private readonly options: PlayerOptions) {
    this.codec = options.codec ?? supportedCodecs()[0] ?? "h264";
    this.muted = options.muted ?? false;
    this.audio = document.createElement("audio");
    this.audio.muted = this.muted;
  }

  async connect(): Promise<void> {
    this.close();
    this.setState("connecting");
    const { video } = this.options;
    const pc = new RTCPeerConnection();
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
      if (s === "connected") this.setState("connected");
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
    if (!this.pc) return null;
    const recent = this.latencies.slice(-60);
    return this.stats.read(this.pc, percentile(recent, 0.5));
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
    if (this.control?.readyState === "open") this.control.send(JSON.stringify(msg) + "\n");
  }

  private sendInput(msg: Record<string, unknown>): void {
    this.send({ t: "input", ...msg });
  }

  private onControlOpen(): void {
    const { video } = this.options;
    this.input = new InputCapture(video, (m) => this.sendInput(m));
    this.gamepads = new GamepadCapture((m) => this.sendInput(m));
    video.focus();
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
        this.probe?.acknowledged(msg.id!, msg.s_us!);
        break;
    }
  }

  private toLocal(serverUs: number): number | null {
    return this.offset ? serverUs / 1000 + this.offset.ms : null;
  }

  private onFrame(md: VideoFrameCallbackMetadata): void {
    this.probe?.onFrame(md);
    const sent = md.rtpTimestamp === undefined ? undefined : this.sentAt.get(md.rtpTimestamp);
    const at = sent === undefined ? null : this.toLocal(sent);
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
export function stereoOpus(sdp: string): string {
  const pt = /a=rtpmap:(\d+) opus\/48000\/2/i.exec(sdp)?.[1];
  if (!pt) return sdp;
  return sdp.replace(new RegExp(`a=fmtp:${pt} ([^\\r\\n]*)`), (line, params: string) =>
    /(^|;)\s*stereo=/.test(params) ? line : `${line};stereo=1`,
  );
}
