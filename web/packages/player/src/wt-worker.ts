// The WebTransport session (`cha-stream/1`), off the main thread: connects to
// the first of the streamer's URLs that answers, reassembles video frames from
// datagrams and hands them over in order, passes audio packets through, and
// relays the control stream's JSON lines both ways.
//
// A frame that can't be completed (lost datagrams, or a gap in frame ids) means
// the decoder would show garbage (H.264 and AV1 decoders don't even say so):
// drop frames until one that doesn't depend on it. First ask the streamer to
// refer around it (reference invalidation: its next frame, flagged RECOVERY,
// refers only to frames before the lost one, which every decoder in Chrome
// takes, bit-exact; spike S8), and if nothing comes of that, for a keyframe.
//
// PyroWave (flagged INTRA: every frame stands alone) is different: its
// datagrams carry packets that each decode on their own (one split over
// several is flagged CONTINUES and usable only whole). Frames go out as soon
// as they're complete, and one still missing datagrams at its deadline goes
// out with the packets that did arrive: a loss blurs a region, nothing more.
//
// FEC: a frame's parity fragments (flagged PARITY, after its data) rebuild
// lost data fragments as soon as every block has enough shards (fec.ts), so
// a lost packet costs nothing more than the parity's overhead.
//
// A codec switch starts a new stream (the header's stream byte): everything
// of the old one is dropped, and the new one starts from its first keyframe.
//
// Every 100 ms the worker reports to the streamer's rate control (plan §3.1
// rule 1, receiver truth): how long frames took from send to complete here
// (the median of the last 150 ms), what arrived (the last 200 ms), and the
// share of a frame's datagrams, data and parity, that never did (frames
// settled in the last second). A frame settles REORDER_MS after its first
// datagram, and stragglers count until then: a frame FEC rebuilt before its
// late fragment came lost nothing, and QUIC's own loss count has packets it
// gave up on that came, reordered. The worker does it because its timers
// keep running when the page is hidden.

import { BLOCK, recover } from "./fec";

export type ToWorker =
  | { type: "start"; urls: string[]; certHash: string }
  | { type: "send"; line: string }
  /** Local clock − server clock (ms), as the page's pings estimate it. */
  | { type: "clock"; offsetMs: number }
  | { type: "close" };

export type FromWorker =
  | { type: "ready"; url: string }
  | { type: "line"; line: string }
  | {
      type: "video";
      /** The stream (codec switch) the frame belongs to. */
      stream: number;
      id: number;
      key: boolean;
      sendTs: number;
      firstAt: number;
      lastAt: number;
      data: ArrayBuffer;
      /** Intra only: some of the frame's packets were lost. */
      partial?: boolean;
    }
  | { type: "audio"; id: number; sendTs: number; data: ArrayBuffer }
  | { type: "lost"; frames: number }
  /** Frames rebuilt from parity (FEC) since the last such message. */
  | { type: "recovered"; frames: number }
  | { type: "bytes"; bytes: number }
  | { type: "closed"; reason: string };

const HEADER_LEN = 16;
const KIND_VIDEO = 0;
const KIND_AUDIO = 1;
const FLAG_KEYFRAME = 1;
const FLAG_PARITY = 1 << 2;
const FLAG_CONTINUES = 1 << 3;
const FLAG_CONTINUED = 1 << 4;
const FLAG_INTRA = 1 << 5;
const FLAG_RECOVERY = 1 << 6;
/** Intra: a frame still missing datagrams this long after its first goes out with what came. */
const FRAME_DEADLINE_MS = 60;
/**
 * A frame missing datagrams counts as lost once a later frame completed
 * ORDER_WAIT_MS ago (its datagrams were all sent before the later frame's),
 * or once none came for this long (the stream went quiet). A stall, the
 * browser busy for a moment, delivers everything late but in order: waited
 * out, it costs no keyframe.
 */
const SILENCE_MS = 250;
/** How long a complete frame waits for an earlier one still arriving. */
const ORDER_WAIT_MS = 15;
const KEYFRAME_RETRY_MS = 250;
/** How long a frame's datagrams may straggle before the missing count as lost. */
const REORDER_MS = 200;
const CONNECT_TIMEOUT_MS = 2500;
/** Complete frames kept while waiting for a keyframe. */
const MAX_WAITING = 240;

interface Partial {
  total: number;
  received: number;
  parts: (Uint8Array | undefined)[];
  /** FEC: parity fragments per block, those that came, a data shard's length and the frame's. */
  fec: number;
  parity: (Uint8Array | undefined)[];
  shardLen: number;
  frameLen: number;
  /** Intra: the part's packet goes on in the next one / came from the previous. */
  continues: boolean[];
  continued: boolean[];
  bytes: number;
  key: boolean;
  /** Refers only to frames before the one this page reported lost. */
  recovery: boolean;
  sendTs: number;
  firstAt: number;
  lastAt: number;
}

interface Complete {
  key: boolean;
  recovery: boolean;
  sendTs: number;
  firstAt: number;
  lastAt: number;
  data: Uint8Array<ArrayBuffer>;
}

const post = (msg: FromWorker, transfer: Transferable[] = []) => self.postMessage(msg, { transfer });
const now = () => performance.timeOrigin + performance.now();

let session: Session | null = null;
/** Local clock − server clock (ms), once the page knows it. */
let clockMs: number | null = null;

/** When a frame stamped `ts` (server µs, 32 bits) was sent, on our clock. */
function sentAt(ts: number): number | null {
  if (clockMs === null) return null;
  const nowUs = (now() - clockMs) * 1000;
  const span = 2 ** 32;
  // Just behind the server's now, or a little ahead (the estimate assumes
  // symmetric paths).
  const behind = ((nowUs % span) - ts + span) % span;
  const serverUs = behind > span / 2 ? nowUs + (span - behind) : nowUs - behind;
  return serverUs / 1000 + clockMs;
}

self.onmessage = (e: MessageEvent<ToWorker>) => {
  const msg = e.data;
  if (msg.type === "start") {
    void start(msg.urls, msg.certHash);
  } else if (msg.type === "send") {
    session?.send(msg.line);
  } else if (msg.type === "clock") {
    clockMs = msg.offsetMs;
  } else if (msg.type === "close") {
    session?.close("closed by the page");
  }
};

async function start(urls: string[], certHash: string): Promise<void> {
  const hash = hexToBytes(certHash);
  for (const url of urls) {
    const wt = new WebTransport(url, {
      serverCertificateHashes: [{ algorithm: "sha-256", value: hash }],
      requireUnreliable: true,
    });
    const timeout = new Promise<"timeout">((r) => setTimeout(() => r("timeout"), CONNECT_TIMEOUT_MS));
    const ready = await Promise.race([wt.ready.then(() => "ready" as const), timeout]).catch(() => "failed" as const);
    if (ready !== "ready") {
      try {
        wt.close();
      } catch {
        // Never opened.
      }
      continue;
    }
    session = new Session(wt);
    await session.open(url);
    return;
  }
  post({ type: "closed", reason: "none of the streamer's addresses answered over WebTransport" });
}

class Session {
  private writer: WritableStreamDefaultWriter<Uint8Array> | null = null;
  private readonly encoder = new TextEncoder();
  private readonly partials = new Map<number, Partial>();
  private readonly complete = new Map<number, Complete>();
  /** The next frame id the decoder may take. */
  private next: number | null = null;
  private needKey = true;
  /** The first frame lost since the decoder's last, and whether the streamer was asked to refer around it. */
  private lostFrom: number | null = null;
  private rfiAsked = false;
  private askedAt = 0;
  private bytes = 0;
  private complained = false;
  /** For the rate-control reports: frames' send → complete, and bytes per 100 ms. */
  private delays: { at: number; ms: number }[] = [];
  /** Each frame's datagrams, expected and come, until it settles. */
  private readonly tally = new Map<number, { at: number; expected: number; got: number }>();
  /** Settled frames' datagrams and how many never came, the last second. */
  private fragments: { at: number; total: number; missing: number }[] = [];
  /** Bytes per report interval, and when each interval began. */
  private arrived: { from: number; bytes: number }[] = [];
  private reportBytes = 0;
  private reportedAt: number | null = null;
  private closed = false;
  private readonly timers: ReturnType<typeof setInterval>[] = [];

  /** The stream being received, and whether its frames stand alone. */
  private stream: number | null = null;
  private intra = false;
  /** Intra: the newest frame handed over. */
  private lastDelivered: number | null = null;

  constructor(private readonly wt: WebTransport) {}

  async open(url: string): Promise<void> {
    try {
      (this.wt.datagrams as { incomingHighWaterMark?: number }).incomingHighWaterMark = 4096;
    } catch {
      // Read-only in some engines.
    }
    const ctl = await this.wt.createBidirectionalStream();
    this.writer = ctl.writable.getWriter();
    post({ type: "ready", url });
    this.wt.closed.then(
      (info) => this.close(info.reason || "the streamer closed the session"),
      (err: unknown) => this.close(`closed: ${String(err)}`),
    );
    this.timers.push(setInterval(() => this.expire(), 10));
    this.timers.push(
      setInterval(() => {
        post({ type: "bytes", bytes: this.bytes });
        this.bytes = 0;
      }, 500),
    );
    this.timers.push(setInterval(() => this.report(), 100));
    void this.readControl(ctl.readable);
    await this.readDatagrams();
  }

  send(line: string): void {
    void this.writer?.write(this.encoder.encode(line.endsWith("\n") ? line : line + "\n")).catch(() => {});
  }

  close(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    this.timers.forEach(clearInterval);
    try {
      this.wt.close();
    } catch {
      // Already closed.
    }
    post({ type: "closed", reason });
  }

  private async readControl(readable: ReadableStream<Uint8Array>): Promise<void> {
    const reader = readable.getReader();
    const text = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read().catch(() => ({ value: undefined, done: true }));
      if (done || !value) return;
      buf += text.decode(value, { stream: true });
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (line) post({ type: "line", line });
      }
    }
  }

  private async readDatagrams(): Promise<void> {
    const reader = this.wt.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read().catch(() => ({ value: undefined, done: true }));
      if (done || !value) return;
      const d = value as Uint8Array;
      this.bytes += d.byteLength;
      this.reportBytes += d.byteLength;
      if (d.byteLength < HEADER_LEN) continue;
      const v = new DataView(d.buffer, d.byteOffset, HEADER_LEN);
      const kind = v.getUint8(0) & 0x0f;
      const id = v.getUint32(4, true);
      const sendTs = v.getUint32(12, true);
      if (kind === KIND_AUDIO) {
        const data = d.slice(HEADER_LEN);
        post({ type: "audio", id, sendTs, data: data.buffer }, [data.buffer]);
        continue;
      }
      if (kind !== KIND_VIDEO) continue;
      // One bad datagram mustn't end the session's video: drop it, say so once.
      try {
        this.fragment(d, v, id, sendTs);
      } catch (err) {
        if (!this.complained) console.error("cha-stream: dropped a video datagram", err instanceof Error ? err.stack : err);
        this.complained = true;
      }
    }
  }

  private fragment(d: Uint8Array, v: DataView, id: number, sendTs: number): void {
    const stream = v.getUint8(2);
    if (stream !== this.stream) {
      // A straggler from before a switch, or the first of a new stream.
      if (this.stream !== null && !newerStream(this.stream, stream)) return;
      this.startStream(stream, (v.getUint8(1) & FLAG_INTRA) !== 0);
    }
    const index = v.getUint16(8, true);
    const total = v.getUint16(10, true);
    const fec = v.getUint8(3);
    const isParity = (v.getUint8(1) & FLAG_PARITY) !== 0 && fec > 0;
    if (total === 0 || (index >= total && !isParity)) return;
    this.count(id, total, fec);
    if (this.complete.has(id)) return;
    if (this.next !== null && before(id, this.next)) return; // already given up on
    if (this.intra && this.lastDelivered !== null && !before(this.lastDelivered, id)) return;
    let f = this.partials.get(id);
    if (!f) {
      f = {
        total,
        received: 0,
        // Dense (filled): array methods skip a sparse array's holes.
        parts: new Array<Uint8Array | undefined>(total).fill(undefined),
        continues: new Array<boolean>(total).fill(false),
        continued: new Array<boolean>(total).fill(false),
        bytes: 0,
        key: (v.getUint8(1) & FLAG_KEYFRAME) !== 0,
        recovery: (v.getUint8(1) & FLAG_RECOVERY) !== 0,
        sendTs,
        firstAt: now(),
        lastAt: 0,
        fec,
        parity: new Array<Uint8Array | undefined>(fec * Math.ceil(total / BLOCK)).fill(undefined),
        shardLen: 0,
        frameLen: 0,
      };
      this.partials.set(id, f);
    }
    f.lastAt = now();
    const payload = d.subarray(HEADER_LEN);
    if (isParity) {
      const p = index - total;
      if (p >= f.parity.length || f.parity[p] || payload.byteLength < 5) return;
      f.frameLen = new DataView(payload.buffer, payload.byteOffset, 4).getUint32(0, true);
      f.parity[p] = payload.slice(4);
      f.shardLen = payload.byteLength - 4;
      if (this.rebuild(f)) this.completeFrame(id, f, true);
      return;
    }
    if (f.parts[index]) return;
    f.parts[index] = payload.slice();
    f.continues[index] = (v.getUint8(1) & FLAG_CONTINUES) !== 0;
    f.continued[index] = (v.getUint8(1) & FLAG_CONTINUED) !== 0;
    f.bytes += payload.byteLength;
    f.received++;
    if (f.received < f.total) {
      if (f.fec > 0 && this.rebuild(f)) this.completeFrame(id, f, true);
      return;
    }
    this.completeFrame(id, f, false);
  }

  /**
   * FEC: rebuilds the missing data fragments once every block has as many
   * shards as data; true if the frame is whole now.
   */
  private rebuild(f: Partial): boolean {
    if (!f.shardLen || f.received >= f.total) return f.received >= f.total;
    const blocks: [number, number][] = [];
    for (let first = 0; first < f.total; first += BLOCK) {
      const n = Math.min(BLOCK, f.total - first);
      const b = first / BLOCK;
      let missing = 0;
      for (let i = first; i < first + n; i++) if (!f.parts[i]) missing++;
      let parity = 0;
      for (let r = 0; r < f.fec; r++) if (f.parity[b * f.fec + r]) parity++;
      if (missing > parity) return false;
      if (missing) blocks.push([first, n]);
    }
    for (const [first, n] of blocks) {
      const b = first / BLOCK;
      const data = f.parts.slice(first, first + n);
      if (!recover(data, f.parity.slice(b * f.fec, (b + 1) * f.fec), f.shardLen)) return false;
      for (let i = 0; i < n; i++) {
        if (!f.parts[first + i]) {
          f.parts[first + i] = data[i];
          f.received++;
        }
      }
    }
    // Rebuilt shards are padded: the frame is exactly frameLen long.
    f.bytes = f.frameLen;
    return true;
  }

  private completeFrame(id: number, f: Partial, rebuilt: boolean): void {
    this.partials.delete(id);
    if (rebuilt) post({ type: "recovered", frames: 1 });
    this.noteDelay(f.sendTs);
    if (this.intra) {
      this.deliverIntra(id, f, false);
      return;
    }
    const data = new Uint8Array(f.bytes);
    let at = 0;
    for (const part of f.parts) {
      // A rebuilt last shard is padded past the frame's end.
      const n = Math.min(part!.byteLength, data.length - at);
      data.set(part!.subarray(0, n), at);
      at += n;
    }
    this.complete.set(id, { key: f.key, recovery: f.recovery, sendTs: f.sendTs, firstAt: f.firstAt, lastAt: now(), data });
    this.deliver();
  }

  private startStream(stream: number, intra: boolean): void {
    this.stream = stream;
    this.intra = intra;
    this.partials.clear();
    this.complete.clear();
    this.tally.clear();
    this.next = null;
    this.needKey = true;
    this.askedAt = 0;
    this.lastDelivered = null;
    this.lostFrom = null;
    this.rfiAsked = false;
  }

  private noteDelay(ts: number): void {
    const sent = sentAt(ts);
    if (sent === null) return;
    const t = now();
    this.delays.push({ at: t, ms: t - sent });
  }

  /** A datagram of frame `id` (`total` data fragments, `fec` parity per block) came. */
  private count(id: number, total: number, fec: number): void {
    let c = this.tally.get(id);
    if (!c) {
      c = { at: now(), expected: total + fec * Math.ceil(total / BLOCK), got: 0 };
      this.tally.set(id, c);
    }
    c.got++;
  }

  /** The rate-control report: delay when frames came, the rate always. */
  private report(): void {
    const t = now();
    this.arrived.push({ from: this.reportedAt ?? t - 100, bytes: this.reportBytes });
    this.reportedAt = t;
    this.reportBytes = 0;
    // About the last 200 ms, over the time it really took (timers run late).
    while (this.arrived.length > 1 && t - this.arrived[0]!.from > 250) this.arrived.shift();
    while (this.delays.length && t - this.delays[0]!.at > 150) this.delays.shift();
    const span = Math.max(t - this.arrived[0]!.from, 1);
    const mbps = (this.arrived.reduce((n, a) => n + a.bytes, 0) * 8) / (span * 1000);
    const sorted = this.delays.map((d) => d.ms).sort((a, b) => a - b);
    const d = sorted.length ? sorted[Math.floor(sorted.length / 2)]! : null;
    // Frames settle in the order their first datagrams came.
    for (const [id, c] of this.tally) {
      if (t - c.at < REORDER_MS) break;
      this.tally.delete(id);
      this.fragments.push({ at: t, total: c.expected, missing: Math.max(0, c.expected - c.got) });
    }
    while (this.fragments.length && t - this.fragments[0]!.at > 1000) this.fragments.shift();
    const total = this.fragments.reduce((n, f) => n + f.total, 0);
    const missing = this.fragments.reduce((n, f) => n + f.missing, 0);
    this.send(
      JSON.stringify({
        t: "report",
        r: Math.round(mbps * 100) / 100,
        ...(d !== null && { d: Math.round(d * 10) / 10 }),
        ...(total > 0 && { l: Math.round((missing / total) * 10_000) / 10_000 }),
      }),
    );
  }

  /** Hands over complete frames in id order, from a keyframe (or a recovery frame) on. */
  private deliver(): void {
    for (;;) {
      if (this.next === null || this.needKey) {
        // Start (again) from the newest complete keyframe, or frame referring
        // around the lost one.
        const lostFrom = this.lostFrom;
        const starts = (id: number, f: Complete) => f.key || (f.recovery && lostFrom !== null && !before(id, lostFrom));
        let key: number | null = null;
        for (const [id, f] of this.complete) if (starts(id, f) && (key === null || before(key, id))) key = id;
        if (key === null) {
          // Frames after a keyframe still arriving are needed; older ones
          // never will be. Keep a bounded window.
          if (this.complete.size > MAX_WAITING) {
            const ids = [...this.complete.keys()].sort((a, b) => (before(a, b) ? -1 : 1));
            for (const id of ids.slice(0, ids.length - MAX_WAITING / 2)) this.complete.delete(id);
          }
          this.askToResync();
          return;
        }
        for (const id of [...this.complete.keys()]) if (before(id, key)) this.complete.delete(id);
        for (const id of [...this.partials.keys()]) if (before(id, key)) this.partials.delete(id);
        this.next = key;
        this.needKey = false;
        this.lostFrom = null;
        this.rfiAsked = false;
      }
      const f = this.complete.get(this.next);
      if (!f) return;
      this.complete.delete(this.next);
      post(
        {
          type: "video",
          stream: this.stream!,
          id: this.next,
          key: f.key,
          sendTs: f.sendTs,
          firstAt: f.firstAt,
          lastAt: f.lastAt,
          data: f.data.buffer,
        },
        [f.data.buffer],
      );
      this.next = (this.next + 1) >>> 0;
    }
  }

  /** Intra: the frame's whole packets, now. */
  private deliverIntra(id: number, f: Partial, partial: boolean): void {
    // A packet starts at a part not CONTINUED and runs through parts that
    // CONTINUE; any part missing, and the packet is dropped.
    const units: Uint8Array[] = [];
    for (let i = 0; i < f.total; i++) {
      const first = f.parts[i];
      if (!first || f.continued[i]) continue;
      const unit = [first];
      let whole = true;
      for (let j = i; f.continues[j]; j++) {
        const next = f.parts[j + 1];
        if (!next || !f.continued[j + 1]) {
          whole = false;
          break;
        }
        unit.push(next);
      }
      if (whole) units.push(...unit);
    }
    const size = units.reduce((n, u) => n + u.byteLength, 0);
    if (!size) return;
    const data = new Uint8Array(size);
    let at = 0;
    for (const u of units) {
      data.set(u, at);
      at += u.byteLength;
    }
    this.lastDelivered = id;
    post(
      { type: "video", stream: this.stream!, id, key: true, sendTs: f.sendTs, firstAt: f.firstAt, lastAt: now(), data: data.buffer, partial },
      [data.buffer],
    );
  }

  /** Gives up on frames that won't complete, or that later ones overtook. */
  private expire(): void {
    if (this.intra) {
      const t = now();
      for (const [id, f] of [...this.partials]) {
        if (t - f.firstAt < FRAME_DEADLINE_MS) continue;
        this.partials.delete(id);
        post({ type: "lost", frames: 1 });
        if (this.lastDelivered === null || before(this.lastDelivered, id)) this.deliverIntra(id, f, true);
      }
      return;
    }
    if (this.next === null || this.needKey) {
      this.deliver();
      if (this.needKey && this.lostFrom !== null) this.retryLostRecovery();
      return;
    }
    const t = now();
    const missing = this.next;
    const partial = this.partials.get(missing);
    const overtaken = [...this.complete.values()].some((f) => t - f.lastAt > ORDER_WAIT_MS);
    // Still arriving (a large keyframe takes a while on a slow link) and not
    // overtaken: wait.
    const stale = overtaken || (partial !== undefined && t - partial.lastAt > SILENCE_MS);
    if (!stale) return;
    // The frame the decoder needs next is gone: so is everything until one
    // that refers around it.
    let lost = 0;
    for (const id of [...this.partials.keys()]) {
      if (before(id, missing)) continue;
      this.partials.delete(id);
      lost++;
    }
    lost += this.complete.size;
    this.complete.clear();
    post({ type: "lost", frames: Math.max(1, lost) });
    this.needKey = true;
    if (this.lostFrom === null) {
      this.lostFrom = missing;
      this.rfiAsked = false;
      this.askedAt = 0;
    }
    this.askToResync();
  }

  /**
   * The recovery frame itself was lost (later frames completed past it, or
   * it went quiet): ask again now, rather than at the retry.
   */
  private retryLostRecovery(): void {
    const t = now();
    for (const [id, f] of this.partials) {
      if (!f.recovery) continue;
      let overtaken = false;
      for (const [later, c] of this.complete) if (before(id, later) && t - c.lastAt > ORDER_WAIT_MS) overtaken = true;
      if (!overtaken && t - f.lastAt <= SILENCE_MS) continue;
      this.partials.delete(id);
      this.rfiAsked = false;
      this.askedAt = 0;
      this.askToResync();
      return;
    }
  }

  /** Asks to refer around the lost frame, then (nothing having come of it) for a keyframe. */
  private askToResync(): void {
    const t = now();
    if (t - this.askedAt < KEYFRAME_RETRY_MS) return;
    this.askedAt = t;
    if (this.lostFrom !== null && !this.rfiAsked) {
      this.rfiAsked = true;
      this.send(JSON.stringify({ t: "rfi", id: this.lostFrom }));
    } else {
      this.send(JSON.stringify({ t: "keyframe" }));
    }
  }
}

/** Stream b came after stream a (8 bits, wrapping). */
function newerStream(a: number, b: number): boolean {
  const d = (b - a) & 0xff;
  return d > 0 && d < 0x80;
}

/** a comes before b, with u32 wraparound. */
function before(a: number, b: number): boolean {
  return ((b - a) >>> 0) < 0x80000000 && a !== b;
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
