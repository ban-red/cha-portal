// Pad state as an input datagram (crates/cha-proto/src/input.rs): byte 0 is
// the wire version and kind (0x12: version 1, `Input`), byte 1 the payload's
// format (0: a pad message as JSON, the control channel's line without its
// newline), then the payload. It goes in a WebTransport datagram or on the
// unordered WebRTC "input" channel.

export const INPUT_BYTE0 = 0x12;
export const PAD_JSON = 0;
/**
 * Largest datagram we send. WebTransport's limit is about 1200 bytes (the path MTU less QUIC's
 * overhead); a pad message is 150 to 400, so a bigger one is a surprise and goes reliably.
 */
export const MAX_INPUT_DATAGRAM = 1000;

const encoder = new TextEncoder();

/** The datagram for a pad message (`{ t: "input", k: "pad", ... }`), or null if it won't fit one. */
export function encodePadDatagram(msg: Record<string, unknown>): Uint8Array | null {
  const json = encoder.encode(JSON.stringify(msg));
  if (json.length + 2 > MAX_INPUT_DATAGRAM) return null;
  const out = new Uint8Array(json.length + 2);
  out[0] = INPUT_BYTE0;
  out[1] = PAD_JSON;
  out.set(json, 2);
  return out;
}

/** The pad message in a datagram, or null if it isn't one. */
export function decodePadDatagram(data: Uint8Array): Record<string, unknown> | null {
  if (data.length < 2 || data[0] !== INPUT_BYTE0 || data[1] !== PAD_JSON) return null;
  try {
    return JSON.parse(new TextDecoder().decode(data.subarray(2)));
  } catch {
    return null;
  }
}

/** Where input datagrams go on the current transport. */
export interface DatagramSink {
  /**
   * Hands the datagram to the transport: true if it is on its way or was dropped for the
   * transport being backed up (the next state, or the resend, replaces it), false if this
   * transport can't carry one now.
   */
  send(data: Uint8Array<ArrayBuffer>): boolean;
}

/**
 * Whether pad state may go as datagrams, and sending it. The streamer says it takes them in its
 * hello (`padDatagrams`; older streamers and the WebSocket one don't), and the page needs a way
 * to send them on the transport it has; with either missing the pad goes on the reliable channel.
 */
export class PadDatagrams {
  /** The streamer's hello said it takes them. */
  accepted = false;
  /** Datagrams handed over, for the stats. */
  sent = 0;

  constructor(private readonly sink: () => DatagramSink | null) {}

  /** True if `msg` (a full `{ t: "input", k: "pad", ... }` message) went as a datagram; false to send it reliably. */
  send(msg: Record<string, unknown>): boolean {
    if (!this.accepted) return false;
    const sink = this.sink();
    if (!sink) return false;
    const data = encodePadDatagram(msg);
    if (!data || !sink.send(data as Uint8Array<ArrayBuffer>)) return false;
    this.sent++;
    return true;
  }
}
