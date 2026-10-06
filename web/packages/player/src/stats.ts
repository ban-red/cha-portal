// A once-a-second summary of the WebRTC stats, for the overlay.

export interface StatsSnapshot {
  codec: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  /** The frame rate the streamer encodes at (60, 90 or 120); null until it says. `fps` is what arrives. */
  targetFps: number | null;
  /**
   * Frames per second the streamer sent, from its `stats` messages; null until two have come
   * in (or on an older streamer). The streamer sends only when the picture changes, so an idle
   * desktop sends almost none: judge `fps` against this, not `targetFps`.
   */
  sentFps: number | null;
  /**
   * The streamer's composited → encoded p99 over its last report, ms: how late
   * the node makes frames, whatever else runs on its GPU. Null until it says.
   */
  encodeP99Ms?: number | null;
  /**
   * Frames per second shown over the same span as `sentFps` (between its first and last report,
   * shifted by the latency), so a burst of sends then a still screen compares like with like.
   * Null without `sentFps`; judge stutter with this and fall back to `fps`.
   */
  shownSentFps: number | null;
  mbps: number | null;
  /** Average decode time per frame over the last interval. */
  decodeMs: number | null;
  /** Average jitter-buffer wait per frame over the last interval. */
  jitterMs: number | null;
  rttMs: number | null;
  packetsLost: number;
  /** Frames rebuilt from FEC parity instead of lost (WebTransport). */
  framesRecovered: number;
  /**
   * PyroWave frames shown with some packets missing (WebTransport): each frame
   * stands alone, so it shows softer where blocks are missing instead of
   * being lost, and the next one is whole again.
   */
  framesPartial: number;
  framesDropped: number;
  /** Server send → shown here, p50 over the last second (clock-synced). */
  latencyMs: number | null;
  /**
   * Server send → decoded, p50 and p95 over the last second (clock-synced;
   * WebTransport only): the network's queueing shows here first.
   */
  deliveryMs: number | null;
  deliveryP95Ms: number | null;
  /**
   * The longest wait between two frames' arrivals over the last second, by
   * the worker's clock (true even when a hidden page handles them late; up
   * to now if none came). WebTransport only.
   */
  frameGapMs: number | null;
  /** Average audio jitter-buffer wait over the last interval (NetEq). */
  audioJitterMs: number | null;
  /** The node's resource use, from the streamer's last report; null if none came in the last 3 s. */
  node: NodeStats | null;
}

/** The node's CPU, RAM and GPU, once a second (the `system` control message). Percent is 0..100. */
export interface NodeStats {
  /** The whole machine's CPU use and its core count; the 1-minute load. */
  cpu: number;
  cores: number;
  load1: number;
  /** Bytes. */
  memUsed: number;
  memTotal: number;
  /** The GPU the streamer encodes on, if the node has NVML; each of these only if it was read. */
  gpu?: number;
  vramUsed?: number;
  vramTotal?: number;
  /** NVENC and NVDEC utilisation. */
  enc?: number;
  dec?: number;
  /** °C, watts and MHz (the SM clock). */
  temp?: number;
  power?: number;
  powerLimit?: number;
  clock?: number;
  /** The streamer's own CPU, in percent of one core (so it may pass 100). */
  streamerCpu: number;
  /** Milliseconds since it arrived. */
  ageMs: number;
}

/** A report older than this isn't shown. */
export const NODE_STATS_FRESH_MS = 3000;

/** A `system` message as node stats (without its age), or null if it lacks the basics. */
export function toNodeStats(msg: Record<string, unknown>): Omit<NodeStats, "ageMs"> | null {
  const num = (k: string) => (typeof msg[k] === "number" && Number.isFinite(msg[k]) ? (msg[k] as number) : undefined);
  const cpu = num("cpu");
  const memTotal = num("mem_total");
  if (cpu === undefined || memTotal === undefined) return null;
  const stats: Omit<NodeStats, "ageMs"> = {
    cpu,
    cores: num("cores") ?? 0,
    load1: num("load1") ?? 0,
    memUsed: num("mem_used") ?? 0,
    memTotal,
    streamerCpu: num("streamer_cpu") ?? 0,
  };
  const optional = { gpu: "gpu", vramUsed: "vram_used", vramTotal: "vram_total", enc: "enc", dec: "dec", temp: "temp", power: "power", powerLimit: "power_limit", clock: "clock" } as const;
  for (const [field, key] of Object.entries(optional)) {
    const v = num(key);
    if (v !== undefined) (stats as unknown as Record<string, number>)[field] = v;
  }
  return stats;
}

interface Counters {
  at: number;
  bytes: number;
  decoded: number;
  decodeTime: number;
  jitterDelay: number;
  emitted: number;
  audioDelay: number;
  audioEmitted: number;
}

export class StatsReader {
  private last: Counters | null = null;

  async read(pc: RTCPeerConnection, latencyMs: number | null): Promise<StatsSnapshot> {
    const report = await pc.getStats();
    let inbound: Record<string, unknown> | null = null;
    let audio: Record<string, unknown> | null = null;
    let rttMs: number | null = null;
    const codecs = new Map<string, string>();
    report.forEach((s: Record<string, unknown>) => {
      if (s.type === "inbound-rtp" && s.kind === "video") inbound = s;
      if (s.type === "inbound-rtp" && s.kind === "audio") audio = s;
      if (s.type === "codec") codecs.set(s.id as string, (s.mimeType as string).replace("video/", ""));
      if (s.type === "candidate-pair" && s.nominated && typeof s.currentRoundTripTime === "number") {
        rttMs = s.currentRoundTripTime * 1000;
      }
    });
    const i = (inbound ?? {}) as Record<string, number | string | undefined>;
    const num = (k: string) => (typeof i[k] === "number" ? (i[k] as number) : 0);
    const now: Counters = {
      at: performance.now(),
      bytes: num("bytesReceived"),
      decoded: num("framesDecoded"),
      decodeTime: num("totalDecodeTime"),
      jitterDelay: num("jitterBufferDelay"),
      emitted: num("jitterBufferEmittedCount"),
      audioDelay: Number((audio as Record<string, unknown> | null)?.jitterBufferDelay ?? 0),
      audioEmitted: Number((audio as Record<string, unknown> | null)?.jitterBufferEmittedCount ?? 0),
    };
    const prev = this.last;
    this.last = now;
    const per = (a: number, b: number) => (b > 0 ? (a / b) * 1000 : null);
    return {
      codec: typeof i.codecId === "string" ? (codecs.get(i.codecId) ?? null) : null,
      width: typeof i.frameWidth === "number" ? i.frameWidth : null,
      height: typeof i.frameHeight === "number" ? i.frameHeight : null,
      fps: typeof i.framesPerSecond === "number" ? i.framesPerSecond : null,
      targetFps: null,
      sentFps: null,
      shownSentFps: null,
      mbps: prev ? ((now.bytes - prev.bytes) * 8) / ((now.at - prev.at) * 1000) : null,
      decodeMs: prev ? per(now.decodeTime - prev.decodeTime, now.decoded - prev.decoded) : null,
      jitterMs: prev ? per(now.jitterDelay - prev.jitterDelay, now.emitted - prev.emitted) : null,
      rttMs,
      packetsLost: num("packetsLost"),
      framesDropped: num("framesDropped"),
      latencyMs,
      deliveryMs: null,
      deliveryP95Ms: null,
      frameGapMs: null,
      framesRecovered: 0,
      framesPartial: 0,
      node: null,
      audioJitterMs: prev ? per(now.audioDelay - prev.audioDelay, now.audioEmitted - prev.audioEmitted) : null,
    };
  }
}
