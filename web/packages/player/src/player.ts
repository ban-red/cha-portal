// One connection to an environment's streamer, over one of three transports:
// - WebTransport (`cha-stream/1`, the Chromium fast path): video and audio
//   datagrams reassembled in a worker, decoded with WebCodecs and shown through
//   track generators (S1d: the fastest way to the screen);
// - WebSocket (ADR 0022): the same datagrams and control lines as messages on one
//   TCP connection, through the portal, for guests who can't reach the node
//   (an internet share link); decoded by the same path as WebTransport;
// - WebRTC everywhere else: recvonly video (playout-delay 0) and stereo Opus.
// Video and audio are separate MediaStreams, so video never waits for lip
// sync. The control channel (a DataChannel, or WebTransport's first stream)
// carries input, resize requests and clock pings up, and per-frame send times
// down. The portal brokers the session; media flows straight from the node.

import { AudioJitter } from "./audio-jitter";
import { AudioOut } from "./audio-out";
import { presenceActive } from "./presence";
import { ControllerManager, type ManagedController } from "./controllers";
import type { CaptureView } from "./captureMode";
import { InputCapture } from "./input";
import { judgeLiveness, RESUME_ANSWER_MS, shouldReconnectOnResume } from "./liveness";
import { overlayAnswer, parseOverlay, type OverlayLevel, type OverlayState } from "./overlay";
import { ClickProbe, percentile, type ProbeResult } from "./probe";
import { NODE_STATS_FRESH_MS, StatsReader, toNodeStats, type NodeStats, type StatsSnapshot } from "./stats";
import { PyroPresenter } from "./pyro";
import type { FromWorker, ToWorker } from "./reassembly";
import { transportOrder } from "./transports";
import { resolveWsUrl } from "./ws-url";
import { capturesDevices, inputAllowed, readsPads, sendsControls, type InputMode } from "./inputMode";
import { parseWatchers, type Watcher } from "./watchers";

/** Hardware codecs (every transport) and PyroWave (WebTransport only). */
export type Codec = "hevc" | "h264" | "av1" | "pyrowave420" | "pyrowave444";

export function isPyroWave(codec: Codec): codec is "pyrowave420" | "pyrowave444" {
  return codec === "pyrowave420" || codec === "pyrowave444";
}
export type PlayerState = "idle" | "connecting" | "connected" | "disconnected" | "failed";

export type Transport = "webrtc" | "webtransport" | "websocket";

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

/** WebSocket media needs the same WebCodecs and track generators as WebTransport. */
export function supportsWebSocket(): boolean {
  return (
    typeof WebSocket !== "undefined" &&
    typeof VideoDecoder !== "undefined" &&
    typeof AudioDecoder !== "undefined" &&
    "MediaStreamTrackGenerator" in globalThis
  );
}

/** Where to reach the streamer over WebSocket (from the portal): URLs, absolute or relative to the page. */
export interface WebSocketOffer {
  urls: string[];
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
  /** Asks the portal for the streamer's WebSocket URLs (ADR 0022); without it, no WebSocket. */
  webSocket?: (codec: Codec) => Promise<WebSocketOffer>;
  /** "auto" (the default): WebTransport where the browser has it, else WebRTC. One transport: only that one. */
  transport?: "auto" | Transport;
  /**
   * The transports to try, in order; the next is tried when one fails to connect. Each is skipped
   * when the browser lacks it or its option (`webTransport`, `webSocket`) is missing. Overrides
   * `transport`. Default `["webtransport", "webrtc"]`. PyroWave always uses WebTransport.
   */
  transports?: Transport[];
  /** How long WebRTC may take to connect when another transport follows it (default 6000 ms). */
  webrtcTimeoutMs?: number;
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
  onFloor?: (control: boolean, viewers: number, player?: number, canTake?: boolean) => void;
  /**
   * The other sessions on the environment, heard only while this page has the controls (an owner
   * or admin): to list who's watching and hand the controls to a guest controller (`giveControl`).
   * Sent when someone joins or leaves or the controls move.
   */
  onViewers?: (list: Watcher[]) => void;
  /**
   * What the page sends up. `"all"` (the default): keyboard, mouse, controllers and the rest, as
   * far as it has the controls. `"pads"` (a share link's guest, ADR 0014): only controllers, with
   * their feedback; no keyboard, mouse, wheel, pointer lock, clipboard or resize. `"none"` (a viewer
   * link's guest, ADR 0015): nothing at all, not even controllers.
   */
  input?: InputMode;
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
  /**
   * The performance overlay's level (0 off to 4 full, or `"custom"`), when the streamer first says
   * and whenever it changes, whoever changed it; `null` when the app has no overlay.
   */
  onOverlay?: (level: OverlayState | null) => void;
  /** The controllers this page sends (Gamepad API or WebHID), now and whenever one arrives or leaves. */
  onControllers?: (controllers: ManagedController[]) => void;
  /**
   * Mouse capture: `recapture` is on after the browser released the mouse (Esc) and a click on
   * the picture captures it again; `hint` asks for "Click to capture the mouse" to be shown.
   */
  onMouseCapture?: (view: CaptureView) => void;
  /**
   * Esc is held with the mouse captured (Keyboard Lock, full screen): the progress, 0..1, towards
   * letting go of the mouse once the hint is due, and null when no hint should show.
   */
  onEscHold?: (progress: number | null) => void;
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
  /** Floor: this session could take the controls now (a guest controller's "Take control"). */
  can_take?: boolean;
  /** Viewers: the other sessions, `{id, role, slot?}`. */
  list?: unknown;
  /** A share link's player: the pad index it plays on (1 is "player 2"). */
  player?: number;
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
  /** Overlay: the level now, in an answer; stats and the hello carry it as `overlay`. */
  level?: unknown;
  overlay?: unknown;
  /** Cumulative frames the streamer sent (`stats`). */
  frames_sent?: number;
  /** Composited → encoded p99 over the streamer's last report, ms (`stats`). */
  composite_to_encoded_ms_p99?: number | null;
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
  /** PyroWave: its WebGPU device was already replaced once; a second loss reconnects. */
  pyroRebuilt?: boolean;
}

/** How long the WebTransport worker may take to open the session before the player gives up on it. */
const WT_READY_TIMEOUT_MS = 10_000;
/** How long WebRTC may take to connect before the next transport is tried. */
const WEBRTC_TIMEOUT_MS = 6000;
/** This many video decoder or track failures within `RECOVERY_WINDOW_MS` mean a fresh connection is needed. */
const RECOVERY_LIMIT = 3;
const RECOVERY_WINDOW_MS = 10_000;

/** How long a codec switch may take before the player reconnects instead. */
const SWITCH_TIMEOUT_MS = 3000;
/** How long a frame rate change may take to be answered. */
const FPS_TIMEOUT_MS = 3000;
/** How long an overlay change may take to be answered. */
const OVERLAY_TIMEOUT_MS = 3000;
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
  /** The last `presence` sent on this control channel; null until one is. */
  private presence: boolean | null = null;
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
  /** The performance overlay's level, once the streamer says (null: the app has none). */
  private streamOverlay: OverlayState | null = null;
  /** An overlay change asked for, waiting for the streamer's answer. */
  private overlayChange: { done: (level: OverlayState | null) => void; fail: (err: Error) => void } | null = null;
  /** This page has the controls (streamers before P2.6 don't say: assume so). */
  private hasControl = true;
  /** False while the page's mouse is switched off (the toolbar's button). */
  private mouseEnabled = true;
  /** Where a viewer's page draws the controller's pointer. */
  private pointerEl: HTMLElement | null = null;
  /** The node's latest resource report, and when it came. */
  /** The streamer's composited → encoded p99 from its last `stats` message, ms. */
  private encodeP99Ms: number | null = null;
  /** The streamer's cumulative `frames_sent` at each recent `stats` message, for the send rate. */
  private sentCounts: { at: number; frames: number }[] = [];
  /** Presented-frame counts (requestVideoFrameCallback's `presentedFrames`) by local time, for `shownSentFps`. */
  private presented: { at: number; n: number }[] = [];
  /** The count of the newest entry trimmed from `presented`. */
  private presentedBefore: number | null = null;
  private nodeStats: { stats: Omit<NodeStats, "ageMs">; at: number } | null = null;
  private pointerSpot: { x: number; y: number; drawn: boolean } | null = null;
  /** The environment's latest cursor, and the images seen so far by id. */
  private cursor: ServerMessage | null = null;
  private readonly cursorImages = new Map<string, CursorImage>();
  private readonly latencySinks = new Set<number[]>();
  private readonly audio: HTMLAudioElement;
  private audioTrack: MediaStreamTrack | null = null;
  private muted: boolean;
  transport: Transport | null = null;
  /** When the node last sent anything, and when it last answered a ping (performance.now). */
  private lastInbound = 0;
  private lastPong = 0;
  /** The WebTransport or WebSocket session (`transport` says which): a worker that reassembles frames, and the decode path. */
  private wt: {
    worker: Worker;
    frames: WritableStreamDefaultWriter<VideoFrame>;
    audio: AudioDecoder;
    /**
     * Orders and paces the audio packets (audio-jitter.ts), the timer for the next one's slot, and
     * where the sound goes: the Web Audio output (`out`), or without Web Audio worklets a track
     * writer feeding the `<audio>` element.
     */
    jitter: AudioJitter<ArrayBuffer>;
    jitterTimer: ReturnType<typeof setTimeout> | undefined;
    soundWriter: WritableStreamDefaultWriter<AudioData> | null;
    out: AudioOut | null;
    /**
     * Times the sound was rebuilt: its decoder after an error (a WebCodecs decoder that errs closes
     * for good, and the sound stopped for the rest of the session), its output after it broke (a
     * context that wouldn't resume or closed, a worklet that played nothing) or its track after a
     * failed write, or `restartAudio()`.
     */
    audioRestarts: number;
    /** Times the video decoder or track was rebuilt, and when the recent ones were (performance.now). */
    videoRestarts: number;
    videoFailures: number[];
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
    /** Intra frames shown with packets missing. */
    partial: number;
    recovered: number;
    shown: number[];
    /** Server send → decoded per frame, the last second's. */
    delivery: { at: number; ms: number }[];
    /** When frames arrived (the worker's clock), the last second's and one before. */
    /** Each frame's arrival (epoch ms) and send time (the streamer's µs clock, 32-bit), the last second's. */
    arrivals: { at: number; sendUs: number }[];
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
    this.streamOverlay = null;
    this.setState("connecting");
    const order = transportOrder(this.options, isPyroWave(this.codec), {
      webtransport: !!this.options.webTransport && supportsWebTransport(),
      websocket: !!this.options.webSocket && supportsWebSocket(),
      webrtc: true,
    });
    for (let i = 0; i < order.length; i++) {
      const last = i === order.length - 1;
      try {
        if (order[i] === "webrtc") await this.connectWebRtc(last ? undefined : (this.options.webrtcTimeoutMs ?? WEBRTC_TIMEOUT_MS));
        else await this.connectWorker(order[i] as "webtransport" | "websocket");
        return;
      } catch (err) {
        this.close();
        if (last) {
          if (this.state !== "failed") this.setState("failed", err instanceof Error ? err.message : String(err));
          throw err;
        }
        // Fall back to the next transport.
        this.setState("connecting");
      }
    }
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

  /** The performance overlay's level, once the streamer says; `null` when the app has none. */
  get overlay(): OverlayState | null {
    return this.streamOverlay;
  }

  /**
   * Sets the performance overlay (0 off, 1 FPS only, 2 bar, 3 extended, 4 full): the streamer
   * writes the app's overlay config and the overlay changes within about 100 ms. Resolves with the
   * level now; rejects if this page doesn't have the controls, the app has no overlay, or the
   * streamer refuses or doesn't answer in 3 s.
   */
  setOverlay(level: OverlayLevel): Promise<OverlayState> {
    if (this.state !== "connected") return Promise.reject(new Error("not connected"));
    if (!this.hasControl) return Promise.reject(new Error("this page doesn't have the controls"));
    this.overlayChange?.fail(new Error("another change replaced it"));
    return new Promise<OverlayState>((resolve, reject) => {
      const timer = setTimeout(() => finish(() => reject(new Error("the streamer didn't answer"))), OVERLAY_TIMEOUT_MS);
      const finish = (settle: () => void) => {
        clearTimeout(timer);
        if (this.overlayChange === change) this.overlayChange = null;
        settle();
      };
      const change = {
        done: (now: OverlayState | null) => finish(() => resolve(now ?? level)),
        fail: (err: Error) => finish(() => reject(err)),
      };
      this.overlayChange = change;
      this.send({ t: "overlay", level });
    });
  }

  /** Records the overlay level the streamer reports (hello, stats, or the answer to `setOverlay`). */
  private noteOverlay(level: OverlayState | null): void {
    if (level === this.streamOverlay) return;
    this.streamOverlay = level;
    this.options.onOverlay?.(level);
  }

  /**
   * Switches the video codec. Over WebTransport or WebSocket the session switches in
   * place: the picture stays up until the first frame in the new codec.
   * Otherwise (WebRTC, or a streamer that doesn't switch) it reconnects.
   */
  async switchCodec(codec: Codec): Promise<void> {
    if (codec === this.codec && this.state === "connected") return;
    const wt = this.wt;
    // PyroWave isn't offered over WebSocket: reconnect (which picks WebTransport).
    if (wt && this.state === "connected" && !wt.switching && !(this.transport === "websocket" && isPyroWave(codec))) {
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

  /**
   * `timeoutMs`: another transport follows, so a failure or silence this long rejects without
   * reporting the state as failed; the caller closes and moves on.
   */
  private async connectWebRtc(timeoutMs?: number): Promise<void> {
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
    // While a fallback follows, a connection that fails before it is up rejects `up` instead of showing as failed.
    let pending: { resolve: () => void; reject: (err: Error) => void } | null = null;
    const up =
      timeoutMs === undefined
        ? null
        : new Promise<void>((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error("WebRTC didn't connect in time")), timeoutMs);
            pending = {
              resolve: () => (clearTimeout(timer), resolve()),
              reject: (err) => (clearTimeout(timer), reject(err)),
            };
          });
    up?.catch(() => undefined);
    pc.onconnectionstatechange = () => {
      const s = pc.connectionState;
      if (s === "connected") {
        this.transport = "webrtc";
        this.options.onTransport?.("webrtc");
        this.setState("connected");
        pending?.resolve();
        pending = null;
      } else if (s === "failed") {
        if (pending) {
          pending.reject(new Error("the connection to the node failed"));
          pending = null;
        } else this.setState("failed", "the connection to the node failed");
      } else if (s === "disconnected" || s === "closed") {
        if (!pending) this.setState("disconnected");
      }
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
      if (timeoutMs === undefined) this.setState("failed", err instanceof Error ? err.message : String(err));
      throw err;
    }
    await up;
  }

  close(): void {
    this.cleanup.splice(0).forEach((f) => f());
    this.closeWorkerTransport();
    this.transport = null;
    this.probe?.stop();
    this.input?.dispose();
    this.input = null;
    this.pads?.stop();
    this.pads = null;
    this.options.onControllers?.([]);
    this.options.onMouseCapture?.({ state: "idle", recapture: false, hint: false });
    this.options.onEscHold?.(null);
    this.nodeStats = null;
    this.sentCounts = [];
    this.presented = [];
    this.presentedBefore = null;
    this.fpsChange?.fail(new Error("the session closed"));
    this.overlayChange?.fail(new Error("the session closed"));
    this.control?.close();
    this.pc?.close();
    this.pc = this.control = null;
    this.audio.srcObject = null;
    this.audioTrack = null;
    this.sentAt.clear();
    // Each session has its own clock (the streamer's session epoch): a new one (a reconnect, or a
    // codec switch over WebRTC) must sync again, or every latency is off by the time between them.
    this.offset = null;
    this.latencies.length = 0;
    this.rtcDelivery.length = 0;
    this.lastSize = "";
    if (this.state !== "idle") this.setState("idle");
  }

  /** Sound on or off. Turning it on from a click also satisfies autoplay. */
  setMuted(muted: boolean): void {
    this.muted = muted;
    this.audio.muted = muted;
    this.wt?.out?.setMuted(muted);
    if (!muted) void this.playAudio();
    this.sendPresence();
  }

  /** Sound's level, 0..1, on this page only (the environment's own volume stays). */
  setVolume(volume: number): void {
    this.audio.volume = clampVolume(volume);
    this.wt?.out?.setVolume(this.audio.volume);
    this.sendPresence();
  }

  private async playAudio(): Promise<void> {
    const out = this.wt?.out;
    if (out) {
      if (!this.muted) out.resume();
      return;
    }
    if (this.muted || !this.audio.srcObject || !this.audio.paused) return;
    try {
      await this.audio.play();
      this.options.onAudioBlocked?.(false);
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotAllowedError") this.options.onAudioBlocked?.(true);
    }
  }

  /** Raw relative mouse (games). Holding Esc lets go of it (Chromium full screen, which locks the keyboard); a tap does elsewhere. */
  lockPointer(): Promise<void> {
    this.input?.hideHint();
    return this.input?.lockPointer() ?? Promise.resolve();
  }

  /** Stop (or resume) sending the mouse to the environment; the keyboard and controllers carry on. */
  setMouseEnabled(on: boolean): void {
    this.mouseEnabled = on;
    this.input?.setMouseEnabled(on);
    this.applyCursor();
    this.placePointer();
  }

  /** Leave recapture mode: clicks go to the stream again. */
  turnOffMouseCapture(): void {
    this.input?.turnOffCapture();
  }

  /** Chromium's Keyboard Lock is on: Esc and browser shortcuts reach the stream. */
  get keyboardLocked(): boolean {
    return this.input?.keyboardLocked ?? false;
  }

  async readStats(): Promise<StatsSnapshot | null> {
    const latencyMs = percentile(this.latencies.slice(-(this.streamFps ?? 60)), 0.5);
    let snapshot: StatsSnapshot;
    if (this.wt) snapshot = this.webTransportStats(latencyMs);
    else if (this.pc) snapshot = await this.stats.read(this.pc, latencyMs);
    else return null;
    const ageMs = this.nodeStats ? performance.now() - this.nodeStats.at : Infinity;
    snapshot.targetFps = this.streamFps;
    const sent = this.sentRates(latencyMs ?? 0);
    snapshot.sentFps = sent?.sent ?? null;
    snapshot.shownSentFps = sent?.shown ?? null;
    snapshot.encodeP99Ms = this.encodeP99Ms;
    snapshot.node = this.nodeStats && ageMs <= NODE_STATS_FRESH_MS ? { ...this.nodeStats.stats, ageMs } : null;
    return snapshot;
  }

  /**
   * Starts keeping every frame's send → shown time (ms, clock-synced) and returns a function that
   * stops and hands them back, for a measurement's true p95 and p99 (the stats snapshot has only a
   * p50 per second).
   */
  collectLatencies(): () => number[] {
    const sink: number[] = [];
    this.latencySinks.add(sink);
    return () => {
      this.latencySinks.delete(sink);
      return sink;
    };
  }

  /**
   * Frames per second the streamer sent over its last few reports, and those shown here over the
   * same span (moved on by the latency, as a frame sent at t shows at t + latency); null with under
   * two reports, or none lately.
   */
  private sentRates(latencyMs: number): { sent: number; shown: number | null } | null {
    const now = performance.now();
    const c = this.sentCounts.filter((s) => now - s.at <= SENT_WINDOW_MS);
    this.sentCounts = c;
    if (c.length < 2) return null;
    const first = c[0]!;
    const last = c[c.length - 1]!;
    const dt = last.at - first.at;
    if (dt <= 0 || last.frames < first.frames) return null;
    const shift = last.at + latencyMs <= now ? latencyMs : 0;
    const from = this.presentedAt(first.at + shift);
    const to = this.presentedAt(last.at + shift);
    return {
      sent: ((last.frames - first.frames) * 1000) / dt,
      shown: from === null || to === null ? null : ((to - from) * 1000) / dt,
    };
  }

  /** Frames presented by local time `t`; null before the first. */
  private presentedAt(t: number): number | null {
    let n = this.presentedBefore;
    for (const p of this.presented) {
      if (p.at > t) return n ?? p.n - 1;
      n = p.n;
    }
    return n;
  }

  /** Click → screen, `count` synthetic clicks (use with the test pattern). */
  async runProbe(count = 25): Promise<ProbeResult> {
    const probe = new ClickProbe(
      this.options.video,
      (m) => this.sendInput(m),
      (us) => this.toLocal(us),
      this.muted || this.wt?.out ? null : this.audioTrack,
      this.muted ? null : (this.wt?.out?.tap() ?? null),
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

  /** The link is gone: say so, and the page reconnects with a fresh pipeline. */
  private lose(detail: string): void {
    if (this.state === "connected") this.setState("disconnected", detail);
  }

  /**
   * Watches for a node that went quiet (a dead Wi-Fi link, a sleeping laptop) without the transport
   * saying so. The node answers a ping every second and sends other messages, so four seconds of
   * nothing while the page is in front is a dead link; a still screen is no reason, since video
   * isn't what is counted. A page that comes back to the front with an old last answer (a hidden
   * tab's timers are throttled) gets a fresh grace period and pings at once: no answer within
   * RESUME_ANSWER_MS, and it reconnects. A quick look at another tab costs nothing.
   */
  private watchLiveness(): void {
    this.lastInbound = this.lastPong = performance.now();
    let tickedAt = performance.now();
    const timer = setInterval(() => {
      const t = performance.now();
      const verdict = judgeLiveness({
        silentMs: t - this.lastInbound,
        tickGapMs: t - tickedAt,
        visible: document.visibilityState === "visible",
      });
      tickedAt = t;
      if (verdict === "frozen") this.lastInbound = t; // slept: give the link a fresh few seconds to prove itself
      else if (verdict === "dead") this.lose("no data from the node");
    }, 1000);
    let resumeCheck: ReturnType<typeof setTimeout> | undefined;
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      const asked = performance.now();
      if (!shouldReconnectOnResume(asked - this.lastPong)) return;
      // Judge the link from now, not from the hidden tab's stale numbers.
      this.lastInbound = asked;
      tickedAt = asked;
      this.send({ t: "ping", c: performance.timeOrigin + asked });
      clearTimeout(resumeCheck);
      resumeCheck = setTimeout(() => {
        if (this.lastPong < asked && document.visibilityState === "visible") this.lose("no data from the node");
      }, RESUME_ANSWER_MS);
    };
    document.addEventListener("visibilitychange", onVisible);
    this.cleanup.push(() => {
      clearInterval(timer);
      clearTimeout(resumeCheck);
      document.removeEventListener("visibilitychange", onVisible);
    });
  }

  private send(msg: Record<string, unknown>): void {
    if (this.wt) this.wt.worker.postMessage({ type: "send", line: JSON.stringify(msg) } satisfies ToWorker);
    else if (this.control?.readyState === "open") this.control.send(JSON.stringify(msg) + "\n");
  }

  /** The page's sound is playing to someone, on either transport. */
  private audible(): boolean {
    const out = this.wt?.out;
    if (out) return out.audible();
    return !this.muted && this.audio.volume > 0 && this.audio.srcObject !== null && !this.audio.paused;
  }

  /**
   * Tells the node whether anyone is using this page (in front, or heard), when that changes: a
   * hidden, silent tab keeps its link but not its claim on the environment (idle shutoff).
   */
  private sendPresence(): void {
    if (!this.control && !this.wt) return;
    const active = presenceActive(document.visibilityState === "visible", this.audible());
    if (active === this.presence) return;
    this.presence = active;
    this.send({ t: "presence", active });
  }

  private get inputMode(): InputMode {
    return this.options.input ?? "all";
  }

  private sendInput(msg: Record<string, unknown>): void {
    if (inputAllowed(this.inputMode, this.hasControl, msg)) this.send({ t: "input", ...msg });
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

  /** Asks for the controls (owners and admins get them; a guest controller, when nobody or a guest holds them). */
  takeControl(): void {
    this.send({ t: "take_control" });
  }

  /** Hands the controls to a guest controller's session (a `Watcher.id`); only the holder, an owner or admin, can. */
  giveControl(to: number): void {
    this.send({ t: "give_control", to });
  }

  private onControlOpen(): void {
    const { video } = this.options;
    if (capturesDevices(this.inputMode)) {
      this.input = new InputCapture(video, (m) => this.sendInput(m), {
        onPaste: (text) => this.send({ t: "clipboard", text }),
        onCapture: (view) => this.options.onMouseCapture?.(view),
        onEscHold: (progress) => this.options.onEscHold?.(progress),
      });
      this.input.setMouseEnabled(this.mouseEnabled);
    }
    this.pads?.stop();
    if (readsPads(this.inputMode)) {
      const pads = new ControllerManager({ send: (m) => this.sendInput(m) });
      pads.onChange((list) => this.options.onControllers?.(list));
      pads.start();
      this.pads = pads;
    }
    video.focus();
    // Desktop mode draws the cursor here, with no stream delay; a locked
    // pointer (games) leaves it to the picture.
    const cursorMode = () => {
      if (this.hasControl && sendsControls(this.inputMode)) this.send({ t: "cursor", client: !this.input?.locked });
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
    this.watchLiveness();
    this.presence = null;
    this.sendPresence();
    // Visibility and mute changes say so at once; the sound starting or stopping by itself
    // (autoplay unblocked, the output rebuilt) is caught by the timer.
    const presence = () => this.sendPresence();
    document.addEventListener("visibilitychange", presence);
    const presenceTimer = setInterval(presence, 2000);
    this.cleanup.push(() => {
      document.removeEventListener("visibilitychange", presence);
      clearInterval(presenceTimer);
    });
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
    if (this.options.fixedSize || !sendsControls(this.inputMode)) return;
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
    this.lastInbound = performance.now();
    switch (msg.t) {
      case "pong": {
        this.lastPong = this.lastInbound;
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
        if (typeof msg.text === "string" && sendsControls(this.inputMode)) void this.copied(msg.text);
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
          if (sendsControls(this.inputMode)) this.send({ t: "cursor", client: !this.input?.locked });
          // Pads were sent nothing while the controls were elsewhere.
          this.pads?.resync();
        }
        this.applyCursor();
        this.placePointer();
        this.options.onFloor?.(
          this.hasControl,
          msg.viewers ?? 1,
          typeof msg.player === "number" ? msg.player : undefined,
          msg.can_take === true,
        );
        if (!this.hasControl) this.options.onViewers?.([]);
        break;
      }
      case "viewers":
        if (this.hasControl) this.options.onViewers?.(parseWatchers(msg.list));
        break;
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
        this.noteOverlay(parseOverlay((msg.stream as unknown as { overlay?: unknown } | undefined)?.overlay));
        break;
      case "stats":
        this.noteFps(msg.fps);
        this.noteOverlay(parseOverlay(msg.overlay));
        this.encodeP99Ms = typeof msg.composite_to_encoded_ms_p99 === "number" ? msg.composite_to_encoded_ms_p99 : null;
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
      case "overlay": {
        // The answer to `setOverlay`: the level now (none without an overlay), with an `error` if it didn't change.
        const answer = overlayAnswer(msg);
        this.noteOverlay(answer.level);
        const change = this.overlayChange;
        if (change) {
          if (answer.error) change.fail(new Error(answer.error));
          else change.done(answer.level);
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
    if (!this.mouseEnabled) {
      video.style.cursor = "not-allowed";
      return;
    }
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

  /** WebTransport or WebSocket: a worker receives the datagrams or messages, the page decodes. */
  private async connectWorker(kind: "webtransport" | "websocket"): Promise<void> {
    const label = kind === "webtransport" ? "WebTransport" : "WebSocket";
    const offer: { urls: string[]; certHash?: string } =
      kind === "webtransport" ? await this.options.webTransport!(this.codec) : await this.options.webSocket!(this.codec);
    const { video } = this.options;
    const worker =
      kind === "webtransport"
        ? new Worker(new URL("./wt-worker.ts", import.meta.url), { type: "module" })
        : new Worker(new URL("./ws-worker.ts", import.meta.url), { type: "module" });
    const frames = new MediaStreamTrackGenerator<VideoFrame>({ kind: "video" });
    // Web Audio plays the sound; without it the old track and `<audio>` element do.
    const sound = AudioOut.supported() ? null : new MediaStreamTrackGenerator<AudioData>({ kind: "audio" });
    const wt: NonNullable<Player["wt"]> = {
      worker,
      frames: frames.writable.getWriter(),
      // Made just below, once `wt` exists for their callbacks.
      audio: null as unknown as AudioDecoder,
      jitter: new AudioJitter<ArrayBuffer>(),
      jitterTimer: undefined,
      soundWriter: sound?.writable.getWriter() ?? null,
      out: null,
      audioRestarts: 0,
      videoRestarts: 0,
      videoFailures: [],
      stream: 0,
      pipeline: null as unknown as VideoPipeline,
      switching: null,
      sent: new Map(),
      decodeMs: [],
      bytes: 0,
      bytesAt: performance.now(),
      mbps: null,
      lost: 0,
      partial: 0,
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
    wt.audio = this.newAudioDecoder(wt);
    this.wt = wt;
    video.srcObject = new MediaStream([frames]);
    video.muted = true;
    video.playsInline = true;
    void video.play().catch(() => {});
    if (sound) {
      this.audioTrack = sound;
      this.audio.srcObject = new MediaStream([sound]);
    } else {
      wt.out = this.newAudioOut(wt);
    }
    void this.playAudio();

    const ready = new Promise<void>((resolve, reject) => {
      // The worker failing (a script error, a message it can't read) or never answering ends the
      // attempt: before it opened, connect() falls back to WebRTC; after, the page reconnects.
      const timer = setTimeout(() => reject(new Error(`${label} didn't open in time`)), WT_READY_TIMEOUT_MS);
      const fail = (reason: string) => {
        clearTimeout(timer);
        if (this.wt !== wt) return;
        this.lose(reason);
        reject(new Error(reason));
      };
      worker.onerror = (e) => {
        e.preventDefault();
        fail(`the ${label} worker failed${e.message ? `: ${e.message}` : ""}`);
      };
      worker.onmessageerror = () => fail(`the ${label} worker sent a message the page couldn't read`);
      worker.onmessage = (e: MessageEvent<FromWorker>) => {
        const msg = e.data;
        switch (msg.type) {
          case "ready": {
            clearTimeout(timer);
            this.lastInbound = performance.now();
            this.transport = kind;
            this.options.onTransport?.(kind);
            this.onControlOpen();
            this.setState("connected");
            resolve();
            break;
          }
          case "line":
            this.onServerMessage(msg.line);
            break;
          case "video":
            this.lastInbound = performance.now();
            if (msg.stream !== wt.stream) {
              // The first frame in the codec switched to; stragglers of an
              // older stream (the worker drops most) are ignored.
              if (!wt.switching || !newerStream(wt.stream, msg.stream)) break;
              this.adoptStream(wt, msg.stream);
            }
            this.decodeVideo(wt, msg);
            break;
          case "audio":
            this.lastInbound = performance.now();
            wt.jitter.push(msg.id, msg.sendTs, msg.at, msg.data);
            this.drainAudio(wt);
            break;
          case "lost":
            wt.lost += msg.frames;
            break;
          case "partial":
            wt.partial += msg.frames;
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
            clearTimeout(timer);
            if (this.wt !== wt) break;
            if (this.state === "connected") this.setState("disconnected", msg.reason);
            reject(new Error(msg.reason));
            break;
        }
      };
    });
    // A worker's own location is its script's URL, which a bundler may put
    // elsewhere: resolve the portal's relative paths against the page.
    const urls = kind === "websocket" ? offer.urls.map((u) => resolveWsUrl(u, location.href)) : offer.urls;
    worker.postMessage({ type: "start", urls, certHash: offer.certHash ?? "" } satisfies ToWorker);
    if (this.offset) worker.postMessage({ type: "clock", offsetMs: this.offset.ms } satisfies ToWorker);
    await ready;
  }

  /** Writes a decoded frame to the video track; a failed write means the track broke, so it is rebuilt. */
  private writeFrame(wt: NonNullable<Player["wt"]>, frame: VideoFrame): void {
    const writer = wt.frames;
    writer.write(frame).catch(() => {
      try {
        frame.close();
      } catch {
        // Already closed.
      }
      if (this.wt === wt && wt.frames === writer) this.rebuildVideoTrack(wt);
    });
  }

  /** Counts a video failure; true once there have been too many lately for patching to be worth it. */
  private tooManyVideoFailures(wt: NonNullable<Player["wt"]>): boolean {
    const t = performance.now();
    wt.videoFailures = wt.videoFailures.filter((at) => t - at < RECOVERY_WINDOW_MS);
    wt.videoFailures.push(t);
    wt.videoRestarts++;
    return wt.videoFailures.length >= RECOVERY_LIMIT;
  }

  /** A new video track for the `<video>` element, and playback restarted on it (as `rebuildSoundTrack` does for sound). */
  private rebuildVideoTrack(wt: NonNullable<Player["wt"]>): void {
    if (this.tooManyVideoFailures(wt)) return this.lose("the video track keeps failing");
    try {
      const old = wt.frames;
      const frames = new MediaStreamTrackGenerator<VideoFrame>({ kind: "video" });
      wt.frames = frames.writable.getWriter();
      void old.close().catch(() => undefined);
      const { video } = this.options;
      video.srcObject = new MediaStream([frames]);
      void video.play().catch(() => {});
      this.send({ t: "keyframe" });
    } catch {
      this.lose("the video track couldn't restart");
    }
  }

  /** The WebGPU device behind PyroWave was lost, or decoding threw: one new presenter, else reconnect. */
  private recoverPyro(wt: NonNullable<Player["wt"]>, p: VideoPipeline, reason: string): void {
    if (p.closed || this.wt !== wt || (wt.pipeline !== p && wt.switching?.pipeline !== p) || !p.pyro) return;
    const dead = p.pyro;
    p.pyro = null; // frames wait for the replacement
    setTimeout(() => {
      try {
        dead.destroy();
      } catch {
        // Already lost.
      }
    }, 1000);
    if (p.pyroRebuilt) return this.lose(`PyroWave's GPU failed again: ${reason}`);
    p.pyroRebuilt = true;
    wt.videoRestarts++;
    PyroPresenter.create().then(
      (next) => {
        if (p.closed || this.wt !== wt) return next.destroy();
        next.onLost = (why) => this.recoverPyro(wt, p, why);
        p.pyro = next;
        this.send({ t: "keyframe" }); // frames stand alone, but the next one should come soon
      },
      (err: unknown) => this.lose(`PyroWave's GPU couldn't restart: ${err instanceof Error ? err.message : String(err)}`),
    );
  }

  /** A decoder for `codec`'s frames, writing into the session's video track. */
  private async newPipeline(wt: NonNullable<Player["wt"]>, codec: Codec): Promise<VideoPipeline> {
    const p: VideoPipeline = { codec, pyro: null, video: null, sawKey: false, closed: false };
    if (isPyroWave(codec)) {
      p.pyro = await PyroPresenter.create();
      p.pyro.onLost = (reason) => this.recoverPyro(wt, p, reason);
    } else p.video = this.newVideoDecoder(wt, p);
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
        this.writeFrame(wt, frame); // the sink closes it
      },
      error: () => {
        // An error closes the decoder: a new one, from the next keyframe. Too many in a row, or a
        // new one that can't start, and the page reconnects for a fresh pipeline.
        if (p.closed || this.wt !== wt) return;
        if (this.tooManyVideoFailures(wt)) return this.lose("the video decoder keeps failing");
        p.sawKey = false;
        this.send({ t: "keyframe" });
        try {
          p.video = this.newVideoDecoder(wt, p);
        } catch {
          this.lose("the video decoder couldn't restart");
        }
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
    const pyro = p.pyro;
    if (pyro) setTimeout(() => pyro.destroy(), 1000);
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
    wt.arrivals.push({ at: msg.lastAt, sendUs: msg.sendTs });
    while (wt.arrivals.length > 2 && msg.lastAt - wt.arrivals[1]!.at > 1000) wt.arrivals.shift();
    const p = wt.pipeline;
    if (isPyroWave(p.codec)) {
      if (!p.pyro) return; // its GPU is being replaced
      const started = performance.now();
      this.noteSent(wt, msg.id, msg.sendTs, started);
      let frame: VideoFrame | null;
      try {
        frame = p.pyro.decode(new Uint8Array(msg.data), msg.id, msg.partial ?? false);
      } catch (err) {
        this.recoverPyro(wt, p, err instanceof Error ? err.message : String(err));
        return;
      }
      if (frame) {
        this.pushDecodeMs(wt, performance.now() - started);
        this.noteDelivery(wt, msg.sendTs);
        this.writeFrame(wt, frame);
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

  private closeWorkerTransport(): void {
    const wt = this.wt;
    if (!wt) return;
    this.wt = null;
    wt.worker.postMessage({ type: "close" } satisfies ToWorker);
    setTimeout(() => wt.worker.terminate(), 500);
    clearTimeout(wt.jitterTimer);
    if (wt.audio.state !== "closed") wt.audio.close();
    wt.out?.close();
    wt.out = null;
    void wt.soundWriter?.close().catch(() => undefined);
    this.closePipeline(wt.pipeline);
    if (wt.switching) {
      this.closePipeline(wt.switching.pipeline);
      wt.switching.done(new Error("the session closed"));
      wt.switching = null;
    }
  }

  /**
   * An Opus decoder writing to the sound's track. On an error it closes for good, so a new one
   * takes over from the next packet (a few ms of sound lost, not the rest of the session).
   */
  private newAudioDecoder(wt: NonNullable<Player["wt"]>): AudioDecoder {
    const decoder: AudioDecoder = new AudioDecoder({
      output: (data) => this.writeSound(wt, data),
      error: () => {
        if (this.wt !== wt || wt.audio !== decoder) return;
        wt.audioRestarts++;
        wt.audio = this.newAudioDecoder(wt);
      },
    });
    decoder.configure({ codec: "opus", sampleRate: 48_000, numberOfChannels: 2 });
    return decoder;
  }

  /** The Web Audio output for the sound; it says when it is blocked and counts its own rebuilds. */
  private newAudioOut(wt: NonNullable<Player["wt"]>): AudioOut {
    return new AudioOut({
      muted: this.muted,
      volume: this.audio.volume,
      onBlocked: (blocked) => {
        if (this.wt === wt) this.options.onAudioBlocked?.(blocked);
      },
      onRebuild: () => {
        if (this.wt === wt) wt.audioRestarts++;
      },
    });
  }

  /**
   * Plays decoded sound: Web Audio's worklet, or (without it) the track, where a failed write
   * means the track broke, so it is rebuilt.
   */
  private writeSound(wt: NonNullable<Player["wt"]>, data: AudioData): void {
    if (wt.out) {
      wt.out.push(data);
      return;
    }
    const writer = wt.soundWriter;
    if (!writer) {
      data.close();
      return;
    }
    writer.write(data).catch(() => {
      if (this.wt === wt && wt.soundWriter === writer) this.rebuildSoundTrack(wt);
    });
  }

  /** A new sound track for the `<audio>` element, and playback restarted on it. */
  private rebuildSoundTrack(wt: NonNullable<Player["wt"]>): void {
    wt.audioRestarts++;
    const old = wt.soundWriter;
    const sound = new MediaStreamTrackGenerator<AudioData>({ kind: "audio" });
    wt.soundWriter = sound.writable.getWriter();
    void old?.close().catch(() => undefined);
    this.audioTrack = sound;
    this.audio.srcObject = new MediaStream([sound]);
    void this.playAudio();
  }

  /**
   * Rebuilds the sound from scratch, for when it went quiet and the page couldn't tell (the
   * computer's output device changed, say): a new decoder and track on WebTransport, the same
   * track re-attached on WebRTC, and playback restarted either way.
   */
  restartAudio(): void {
    const wt = this.wt;
    if (wt) {
      const old = wt.audio;
      wt.audio = this.newAudioDecoder(wt);
      if (old.state !== "closed") old.close();
      if (wt.out) {
        wt.audioRestarts++;
        wt.out.restart();
        return;
      }
      this.rebuildSoundTrack(wt);
      return;
    }
    if (!this.audioTrack) return;
    this.audio.srcObject = new MediaStream([this.audioTrack]);
    void this.playAudio();
  }

  /** Plays what the jitter buffer says is due, and sets a timer for the next slot. */
  private drainAudio(wt: NonNullable<Player["wt"]>): void {
    clearTimeout(wt.jitterTimer);
    if (this.wt !== wt) return;
    // A decoder that closed without its error reaching us is replaced here too.
    if (wt.audio.state === "closed") {
      wt.audioRestarts++;
      wt.audio = this.newAudioDecoder(wt);
    }
    const now = performance.timeOrigin + performance.now();
    for (const o of wt.jitter.poll(now)) {
      if (o.kind === "play") {
        if (wt.audio.state === "configured") {
          wt.audio.decode(new EncodedAudioChunk({ type: "key", timestamp: o.id * 10_000, data: o.data }));
        }
      } else {
        // WebCodecs has no Opus PLC: 10 ms of silence keeps the sound's timeline and the sink fed.
        this.writeSound(
          wt,
          new AudioData({
            format: "f32-planar",
            sampleRate: 48_000,
            numberOfFrames: 480,
            numberOfChannels: 2,
            timestamp: o.id * 10_000,
            data: new Float32Array(960),
          }),
        );
      }
    }
    const wait = wt.jitter.nextDueMs(performance.timeOrigin + performance.now());
    if (wait !== null) wt.jitterTimer = setTimeout(() => this.drainAudio(wt), Math.max(1, wait));
  }

  /**
   * The longest stall between frames over the last second: how much longer than the streamer's own
   * spacing a frame took to follow the one before it. A still screen sends nothing, and the wait
   * since the last frame isn't counted, so silence from the node is no freeze; a stall shows when
   * the late frame lands (and in the shown rate meanwhile). Null before two frames.
   */
  private frameGap(wt: NonNullable<Player["wt"]>): number | null {
    const now = performance.timeOrigin + performance.now();
    const a = wt.arrivals;
    let gap: number | null = null;
    for (let i = 1; i < a.length; i++) {
      if (now - a[i]!.at > 1000) continue;
      const sentApart = ((a[i]!.sendUs - a[i - 1]!.sendUs) >>> 0) / 1000;
      gap = Math.max(gap ?? 0, a[i]!.at - a[i - 1]!.at - sentApart);
    }
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

  /** What the Web Audio output actually played, for the stats (nothing on the track path). */
  private soundStats(wt: NonNullable<Player["wt"]>): Partial<StatsSnapshot> {
    const s = wt.out?.stats();
    if (!s) return {};
    return {
      audioOutPeak: s.outPeak,
      audioInPeak: s.inPeak,
      audioOutUnderruns: s.underruns,
      audioOutDrops: s.drops,
      audioOutQueueMs: s.queuedMs,
    };
  }

  private webTransportStats(latencyMs: number | null): StatsSnapshot {
    const wt = this.wt!;
    const t = performance.now();
    wt.shown = wt.shown.filter((at) => t - at < 1000);
    const { video } = this.options;
    const quality = video.getVideoPlaybackQuality?.();
    const decode = wt.decodeMs.length ? wt.decodeMs.reduce((a, b) => a + b, 0) / wt.decodeMs.length : null;
    return {
      codec: `${this.codec.toUpperCase()} · ${this.transport === "websocket" ? "WebSocket" : "WebTransport"}`,
      width: video.videoWidth || null,
      height: video.videoHeight || null,
      fps: wt.shown.length,
      targetFps: null,
      sentFps: null,
      shownSentFps: null,
      mbps: wt.mbps,
      decodeMs: decode,
      jitterMs: null,
      rttMs: this.offset?.rtt ?? null,
      packetsLost: wt.lost,
      framesRecovered: wt.recovered,
      framesPartial: wt.partial,
      audioRestarts: wt.audioRestarts,
      framesDropped: quality?.droppedVideoFrames ?? 0,
      latencyMs,
      ...this.deliveryStats(wt),
      frameGapMs: this.frameGap(wt),
      node: null,
      audioJitterMs: wt.jitter.delayMs,
      ...this.soundStats(wt),
    };
  }

  private onFrame(md: VideoFrameCallbackMetadata): void {
    this.probe?.onFrame(md);
    const now = performance.now();
    this.presented.push({ at: now, n: md.presentedFrames });
    while (this.presented.length > 1 && now - this.presented[0]!.at > SENT_WINDOW_MS + 2000) {
      this.presentedBefore = this.presented.shift()!.n;
    }
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
      const latency = performance.timeOrigin + md.presentationTime - at;
      this.latencies.push(latency);
      for (const sink of this.latencySinks) sink.push(latency);
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
