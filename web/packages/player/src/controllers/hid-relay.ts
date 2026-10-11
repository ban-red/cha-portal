// Raw HID relay: a WebHID controller passed through to the environment, so the app that drives the
// virtual device there (Steam) talks to the real firmware (docs/controllers.md, "Raw HID relay").
//
// - Every input report goes up as the device sent it (report id and bytes), as an unreliable
//   datagram where the transport has them and on the control channel otherwise. The page does not
//   parse it, and sends no parsed pad state for the slot while the relay is open.
// - The feature and output reports the app writes, and the feature reports it asks for, come down
//   as `hid` messages. They are performed on the device one at a time, in the order they came
//   (the app's handshake depends on that), and answered on the control channel.
//
// The streamer owns the sequence: `open` is acknowledged (`hid_open`) or refused, and after that the
// streamer answers the app itself whenever this page is slow, fails or goes (docs say how long it waits).

import type { HidDevice } from "./hid-types";
import { COMMAND_FEATURE, ECHOING_COMMANDS, VALVE_VENDOR_ID, receiveReply, relayedWriteAllowed, rewriteBuildTimes, type GetRetry } from "./steam-protocol";

/** A raw report not heard from for this long is sent again (a datagram may have been lost). */
export const RAW_RESEND_MS = 50;
/** After a report's last change it is sent this many more times, then left alone. */
export const RAW_REST_RESENDS = 3;
/** Output reports waiting behind a slow device are dropped past this many (a rumble resend replaces them). */
const MAX_QUEUED_OUTPUTS = 32;
/** A transfer that failed is tried once more after this long. */
const RETRY_MS = 5;
/** Feature reads logged to the console (with their bytes) at the start of a relay. */
const LOGGED_GETS = 12;

/** The Steam Controller's lizard-mode mouse and keyboard reports: not the app's, never sent. */
export const STEAM_LIZARD_REPORTS: ReadonlySet<number> = new Set([0x40, 0x41]);

export interface RelayLink {
  /** A control channel message (`{ k: "hid", ... }`, without the envelope). */
  send(msg: Record<string, unknown>): void;
  /** The report as an unreliable datagram: true if it went that way, false to send it reliably. */
  sendReport(slot: number, q: number, report: Uint8Array): boolean;
}

/** A request from the streamer: what the app wants the controller to do (`ServerMsg::Hid`). */
export interface HidRequest {
  /** What the reply names; 0 for an output that wants none. */
  id: number;
  op: string;
  /** The report id. */
  r: number;
  /** The data without the report id, base64. */
  d?: string;
}

export type RelayState = "closed" | "opening" | "open";

interface Last {
  q: number;
  bytes: Uint8Array;
  at: number;
  left: number;
  /** It went by datagram, so a copy may be needed. */
  unreliable: boolean;
}

/** The hello's `rawHid`: the `vid:pid` strings of the controllers the streamer can carry raw; none if it says nothing sensible. */
export function parseRawHid(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
}

export function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

export function fromBase64(text: string): Uint8Array {
  const s = atob(text);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

export class HidRelay {
  state: RelayState = "closed";
  /** Counts, for the tests and for looking at in a console. */
  readonly stats = { reports: 0, datagrams: 0, requests: 0, failed: 0, droppedOutputs: 0, gets: 0, blocked: 0 };
  private readonly last = new Map<number, Last>();
  private queue: Promise<void> = Promise.resolve();
  private queued = 0;
  private readonly warned = new Set<string>();
  /** The command last written to the command channel, whose reply the next read of it must echo. */
  private lastCommand: number | undefined;

  constructor(
    readonly slot: number,
    private readonly device: HidDevice,
    private readonly link: RelayLink,
    /** The slot's next sequence number (shared with its parsed messages, so numbers only grow). */
    private readonly nextQ: () => number,
    private readonly now: () => number,
    private readonly skip: ReadonlySet<number> = new Set(),
    /** Reads of a feature report are repeated like this (a puck answers late). Default: once. */
    private readonly getRetry?: GetRetry,
  ) {}

  /** Asks the streamer to pass this controller through; `accepted` is the answer. */
  open(): void {
    if (this.state !== "closed") return;
    this.state = "opening";
    this.link.send({ k: "hid", op: "open", i: this.slot, vid: this.device.vendorId, pid: this.device.productId });
  }

  /** The streamer's answer. On yes the relay is open and the reports heard so far go up; true if it is open now. */
  accepted(ok: boolean): boolean {
    if (this.state !== "opening") return this.state === "open";
    if (!ok) {
      this.state = "closed";
      this.last.clear();
      return false;
    }
    this.state = "open";
    // The streamer has nothing of the controller yet: its latest report of each kind, so the app has a state at once.
    for (const rec of this.last.values()) this.transmit(rec);
    return true;
  }

  /** Lets go: the streamer goes back to its own device. Safe to repeat. */
  close(): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.last.clear();
    this.link.send({ k: "hid", op: "close", i: this.slot });
  }

  /** The device sent an input report (without its id). */
  report(reportId: number, data: DataView): void {
    if (this.state === "closed" || this.skip.has(reportId)) return;
    const bytes = new Uint8Array(1 + data.byteLength);
    bytes[0] = reportId;
    bytes.set(new Uint8Array(data.buffer, data.byteOffset, data.byteLength), 1);
    const rec: Last = { q: this.nextQ(), bytes, at: this.now(), left: RAW_REST_RESENDS, unreliable: false };
    this.last.set(reportId, rec);
    if (this.state === "open") this.transmit(rec);
  }

  private transmit(rec: Last): void {
    this.stats.reports++;
    rec.unreliable = this.link.sendReport(this.slot, rec.q, rec.bytes);
    if (rec.unreliable) this.stats.datagrams++;
    else this.link.send({ k: "hid", op: "rep", i: this.slot, q: rec.q, d: toBase64(rec.bytes) });
  }

  /**
   * Call often (the manager's poll): a report that went by datagram and has been quiet for
   * `RAW_RESEND_MS` is sent again, `RAW_REST_RESENDS` times, with the same sequence number (the
   * streamer takes the first copy and ignores the rest). A controller that reports continuously
   * replaces its reports before this matters; one that reports on change has its last report,
   * the release of a button, made as sure as a pad's state is.
   */
  tick(): void {
    if (this.state !== "open") return;
    const now = this.now();
    for (const [id, rec] of this.last) {
      if (!rec.unreliable || rec.left <= 0) {
        if (rec.left <= 0) this.last.delete(id);
        continue;
      }
      if (now - rec.at < RAW_RESEND_MS) continue;
      rec.at = now;
      rec.left--;
      rec.unreliable = this.link.sendReport(this.slot, rec.q, rec.bytes);
      if (!rec.unreliable) this.link.send({ k: "hid", op: "rep", i: this.slot, q: rec.q, d: toBase64(rec.bytes) });
    }
  }

  /** The streamer wants the controller to do something. Done in order; answered when done. */
  request(req: HidRequest): void {
    // While opening is fine: the streamer opened its side before it could ask.
    if (this.state === "closed") {
      // Not ours (any more): say so, so the streamer need not wait out its timeout.
      if (req.id) this.reply(req.id, false);
      return;
    }
    if (!req.id) {
      if (this.queued >= MAX_QUEUED_OUTPUTS) {
        this.stats.droppedOutputs++;
        return;
      }
    }
    this.queued++;
    this.queue = this.queue.then(() => this.perform(req)).finally(() => void this.queued--);
  }

  private async perform(req: HidRequest): Promise<void> {
    this.stats.requests++;
    const data = req.d === undefined ? new Uint8Array(0) : fromBase64(req.d);
    let lastError: unknown;
    if (!["get_feature", "set_feature", "set_output", "output"].includes(req.op)) {
      this.stats.failed++;
      if (req.id) this.reply(req.id, false, undefined, `unknown hid op ${req.op}`);
      return;
    }
    // Defence in depth: the streamer lets only the allowlisted commands and haptic outputs
    // through. A Valve controller is never written to with anything else from here, whatever
    // arrives (a firmware update through a link that can drop could brick it).
    if (this.device.vendorId === VALVE_VENDOR_ID && req.op !== "get_feature" && !relayedWriteAllowed(req.op, req.r, data)) {
      this.stats.failed++;
      this.stats.blocked++;
      const key = `${req.op}:${req.r}:${req.op === "set_feature" ? (data[0] ?? -1) : 0}`;
      if (!this.warned.has(key)) {
        this.warned.add(key);
        console.warn(`hid relay: refused ${req.op} of report ${req.r}${req.op === "set_feature" ? ` command ${(data[0] ?? 0).toString(16)}` : ""} (not on the allowlist)`);
      }
      if (req.id) this.reply(req.id, false, undefined, "not on the relay allowlist");
      return;
    }
    // A transfer that fails is tried once more a moment later: the device may have been busy.
    for (let attempt = 0; attempt < 2; attempt++) {
      if (attempt) await new Promise((r) => setTimeout(r, RETRY_MS));
      try {
        switch (req.op) {
          case "get_feature": {
            // On a command channel the reply starts with the command that asked for it.
            const echo = req.r === COMMAND_FEATURE && this.lastCommand !== undefined && ECHOING_COMMANDS.has(this.lastCommand) ? this.lastCommand : undefined;
            const bytes = await receiveReply(this.device, req.r, echo, this.getRetry);
            // Steam must not see a firmware older than the one it bundles (it would update it here).
            if (this.device.vendorId === VALVE_VENDOR_ID && req.r === 1) rewriteBuildTimes(bytes);
            this.noteGet(req.r, bytes);
            this.reply(req.id, true, bytes);
            return;
          }
          case "set_feature":
            await this.device.sendFeatureReport(req.r, data as BufferSource);
            if (req.r === COMMAND_FEATURE) this.lastCommand = data[0];
            break;
          case "set_output":
          case "output":
            await this.device.sendReport(req.r, data as BufferSource);
            break;
          default:
            throw new Error(`unknown hid op ${req.op}`);
        }
        if (req.id) this.reply(req.id, true);
        return;
      } catch (err) {
        lastError = err;
      }
    }
    this.stats.failed++;
    const why = lastError instanceof Error ? `${lastError.name}: ${lastError.message}` : String(lastError);
    console.warn(`hid relay: ${req.op} of report ${req.r} failed (${why})`);
    if (req.id) this.reply(req.id, false, undefined, why);
  }

  /** The first few feature replies, as the device gave them, so a run that goes wrong can be read. */
  private noteGet(reportId: number, bytes: Uint8Array): void {
    if (this.stats.gets++ >= LOGGED_GETS) return;
    const head = Array.from(bytes.subarray(0, 16), (b) => b.toString(16).padStart(2, "0")).join(" ");
    console.info(`hid relay: feature report ${reportId} read, ${bytes.length} bytes: ${head}`);
  }

  private reply(id: number, ok: boolean, data?: Uint8Array, error?: string): void {
    const msg: Record<string, unknown> = { k: "hid", op: "reply", i: this.slot, id, ok };
    if (data) msg.d = toBase64(data);
    if (error) msg.e = error.slice(0, 200);
    this.link.send(msg);
  }
}
