// A once-a-second summary of the WebRTC stats, for the overlay.

export interface StatsSnapshot {
  codec: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  mbps: number | null;
  /** Average decode time per frame over the last interval. */
  decodeMs: number | null;
  /** Average jitter-buffer wait per frame over the last interval. */
  jitterMs: number | null;
  rttMs: number | null;
  packetsLost: number;
  /** Frames rebuilt from FEC parity instead of lost (WebTransport). */
  framesRecovered: number;
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
      audioJitterMs: prev ? per(now.audioDelay - prev.audioDelay, now.audioEmitted - prev.audioEmitted) : null,
    };
  }
}
