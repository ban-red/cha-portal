// Pad state as an input datagram (crates/cha-proto/src/input.rs): byte 0 is
// the wire version and kind (0x12: version 1, `Input`), byte 1 the payload's
// format (0: a pad message as JSON, the control channel's line without its
// newline), then the payload. It goes in a WebTransport datagram or on the
// unordered WebRTC "input" channel. Format 1 is a raw HID input report of a
// controller passed through to the app (controllers/hid-relay.ts): the slot (1 byte), the
// report's sequence number (4 bytes, big endian), then the report as the device sent it, id first.

export const INPUT_BYTE0 = 0x12;
export const PAD_JSON = 0;
export const HID_REPORT = 1;
/** Bytes before the report in a `HID_REPORT` payload: the slot and the sequence number. */
const HID_REPORT_HEADER = 5;
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

/** The datagram for a raw HID input report (`report` leads with its id), or null if it won't fit one. */
export function encodeHidDatagram(slot: number, q: number, report: Uint8Array): Uint8Array | null {
  if (report.length === 0 || 2 + HID_REPORT_HEADER + report.length > MAX_INPUT_DATAGRAM) return null;
  const out = new Uint8Array(2 + HID_REPORT_HEADER + report.length);
  out[0] = INPUT_BYTE0;
  out[1] = HID_REPORT;
  out[2] = slot & 0xff;
  new DataView(out.buffer).setUint32(3, q >>> 0, false);
  out.set(report, 2 + HID_REPORT_HEADER);
  return out;
}

/** The slot, sequence number and report in a raw HID datagram, or null if it isn't one. */
export function decodeHidDatagram(data: Uint8Array): { slot: number; q: number; report: Uint8Array } | null {
  if (data.length < 2 + HID_REPORT_HEADER + 1 || data[0] !== INPUT_BYTE0 || data[1] !== HID_REPORT) return null;
  return {
    slot: data[2]!,
    q: new DataView(data.buffer, data.byteOffset).getUint32(3, false),
    report: data.subarray(2 + HID_REPORT_HEADER),
  };
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

  /** True if a raw HID report went as a datagram; false to send it reliably. */
  sendHid(slot: number, q: number, report: Uint8Array): boolean {
    if (!this.accepted) return false;
    const sink = this.sink();
    if (!sink) return false;
    const data = encodeHidDatagram(slot, q, report);
    if (!data || !sink.send(data as Uint8Array<ArrayBuffer>)) return false;
    this.sent++;
    return true;
  }
}
