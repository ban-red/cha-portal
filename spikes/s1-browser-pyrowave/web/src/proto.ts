// TypeScript mirror of crates/cha-proto's DatagramHeader (16 bytes, little-endian).
// The real player will use cha-proto compiled to wasm; the spike keeps it inline.

export const HEADER_LEN = 16;
export const WIRE_VERSION = 1;

export const Kind = { Video: 0, Audio: 1, Input: 2, Feedback: 3, Probe: 4 } as const;

export interface DatagramHeader {
  kind: number;
  flags: number;
  stream: number;
  frameId: number;
  fragIndex: number;
  fragCount: number;
  sendTsUs: number;
}

/** Returns null for datagrams that are too short or from another wire version. */
export function decodeHeader(d: Uint8Array): DatagramHeader | null {
  if (d.byteLength < HEADER_LEN) return null;
  const view = new DataView(d.buffer, d.byteOffset, HEADER_LEN);
  const b0 = view.getUint8(0);
  if (b0 >> 4 !== WIRE_VERSION) return null;
  const fragIndex = view.getUint16(8, true);
  const fragCount = view.getUint16(10, true);
  if (fragCount === 0 || fragIndex >= fragCount) return null;
  return {
    kind: b0 & 0x0f,
    flags: view.getUint8(1),
    stream: view.getUint8(2),
    frameId: view.getUint32(4, true),
    fragIndex,
    fragCount,
    sendTsUs: view.getUint32(12, true),
  };
}

export interface TrafficConfig {
  mbps: number;
  fps: number;
  secs: number;
  dgram: number;
}

export function trafficQuery(c: TrafficConfig): string {
  return `mbps=${c.mbps}&fps=${c.fps}&secs=${c.secs}&dgram=${c.dgram}`;
}

/** Control messages from the server (JSON lines). */
export type ServerMsg =
  | {
      t: "hello";
      config: TrafficConfig;
      frame_bytes: number;
      fragments_per_frame: number;
      max_datagram: number;
      total_frames: number;
    }
  | { t: "pong"; c: number; s_us: number }
  | ({ t: "stats"; elapsed_ms: number } & SenderStats & TransportStats)
  | ({ t: "done" } & SenderStats);

export interface SenderStats {
  frames_generated: number;
  frames_sent: number;
  frames_dropped_backpressure: number;
  datagrams_refused: number;
  datagrams_sent: number;
  bytes_sent: number;
}

export interface TransportStats {
  rtt_ms: number | null;
  cwnd: number | null;
  lost_packets: number | null;
  mtu: number | null;
  send_buffer_free: number | null;
  buffered_amount: number | null;
}

/** Splits a byte stream of JSON lines into parsed messages. */
export class LineDecoder {
  private buf = "";
  private readonly text = new TextDecoder();

  push(chunk: Uint8Array | string): ServerMsg[] {
    this.buf += typeof chunk === "string" ? chunk : this.text.decode(chunk, { stream: true });
    const out: ServerMsg[] = [];
    let nl: number;
    while ((nl = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, nl).trim();
      this.buf = this.buf.slice(nl + 1);
      if (line) out.push(JSON.parse(line) as ServerMsg);
    }
    return out;
  }
}
