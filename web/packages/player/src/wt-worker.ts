// The WebTransport session (`cha-stream/1`), off the main thread: connects to
// the first of the streamer's URLs that answers, reassembles video frames from
// datagrams and hands them over in order, passes audio packets through, and
// relays the control stream's JSON lines both ways.
//
// A frame that can't be completed (lost datagrams, or a gap in frame ids) means
// the decoder would show garbage until the next keyframe: drop frames until
// one arrives, and ask the streamer for it.
//
// PyroWave (flagged INTRA: every frame stands alone) is different: its
// datagrams carry packets that each decode on their own (one split over
// several is flagged CONTINUES and usable only whole). Frames go out as soon
// as they're complete, and one still missing datagrams at its deadline goes
// out with the packets that did arrive: a loss blurs a region, nothing more.
//
// A codec switch starts a new stream (the header's stream byte): everything
// of the old one is dropped, and the new one starts from its first keyframe.

export type ToWorker =
  | { type: "start"; urls: string[]; certHash: string }
  | { type: "send"; line: string }
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
  | { type: "bytes"; bytes: number }
  | { type: "closed"; reason: string };

const HEADER_LEN = 16;
const KIND_VIDEO = 0;
const KIND_AUDIO = 1;
const FLAG_KEYFRAME = 1;
const FLAG_CONTINUES = 1 << 3;
const FLAG_CONTINUED = 1 << 4;
const FLAG_INTRA = 1 << 5;
/** A frame still missing datagrams this long after its first counts as lost. */
const FRAME_DEADLINE_MS = 60;
/** How long a complete frame waits for an earlier one still arriving. */
const ORDER_WAIT_MS = 15;
const KEYFRAME_RETRY_MS = 250;
const CONNECT_TIMEOUT_MS = 2500;
/** Complete frames kept while waiting for a keyframe. */
const MAX_WAITING = 240;

interface Partial {
  total: number;
  received: number;
  parts: (Uint8Array | undefined)[];
  /** Intra: the part's packet goes on in the next one / came from the previous. */
  continues: boolean[];
  continued: boolean[];
  bytes: number;
  key: boolean;
  sendTs: number;
  firstAt: number;
}

interface Complete {
  key: boolean;
  sendTs: number;
  firstAt: number;
  lastAt: number;
  data: Uint8Array<ArrayBuffer>;
}

const post = (msg: FromWorker, transfer: Transferable[] = []) => self.postMessage(msg, { transfer });
const now = () => performance.timeOrigin + performance.now();

let session: Session | null = null;

self.onmessage = (e: MessageEvent<ToWorker>) => {
  const msg = e.data;
  if (msg.type === "start") {
    void start(msg.urls, msg.certHash);
  } else if (msg.type === "send") {
    session?.send(msg.line);
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
  private askedAt = 0;
  private bytes = 0;
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
      this.fragment(d, v, id, sendTs);
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
    if (total === 0 || index >= total || this.complete.has(id)) return;
    if (this.next !== null && before(id, this.next)) return; // already given up on
    if (this.intra && this.lastDelivered !== null && !before(this.lastDelivered, id)) return;
    let f = this.partials.get(id);
    if (!f) {
      f = {
        total,
        received: 0,
        parts: new Array<Uint8Array | undefined>(total),
        continues: new Array<boolean>(total).fill(false),
        continued: new Array<boolean>(total).fill(false),
        bytes: 0,
        key: (v.getUint8(1) & FLAG_KEYFRAME) !== 0,
        sendTs,
        firstAt: now(),
      };
      this.partials.set(id, f);
    }
    if (f.parts[index]) return;
    const payload = d.subarray(HEADER_LEN);
    f.parts[index] = payload.slice();
    f.continues[index] = (v.getUint8(1) & FLAG_CONTINUES) !== 0;
    f.continued[index] = (v.getUint8(1) & FLAG_CONTINUED) !== 0;
    f.bytes += payload.byteLength;
    f.received++;
    if (f.received < f.total) return;

    this.partials.delete(id);
    if (this.intra) {
      this.deliverIntra(id, f, false);
      return;
    }
    const data = new Uint8Array(f.bytes);
    let at = 0;
    for (const part of f.parts) {
      data.set(part!, at);
      at += part!.byteLength;
    }
    this.complete.set(id, { key: f.key, sendTs: f.sendTs, firstAt: f.firstAt, lastAt: now(), data });
    this.deliver();
  }

  private startStream(stream: number, intra: boolean): void {
    this.stream = stream;
    this.intra = intra;
    this.partials.clear();
    this.complete.clear();
    this.next = null;
    this.needKey = true;
    this.askedAt = 0;
    this.lastDelivered = null;
  }

  /** Hands over complete frames in id order, from a keyframe on. */
  private deliver(): void {
    for (;;) {
      if (this.next === null || this.needKey) {
        // Start (again) from the newest complete keyframe.
        let key: number | null = null;
        for (const [id, f] of this.complete) if (f.key && (key === null || before(key, id))) key = id;
        if (key === null) {
          // Frames after a keyframe still arriving are needed; older ones
          // never will be. Keep a bounded window.
          if (this.complete.size > MAX_WAITING) {
            const ids = [...this.complete.keys()].sort((a, b) => (before(a, b) ? -1 : 1));
            for (const id of ids.slice(0, ids.length - MAX_WAITING / 2)) this.complete.delete(id);
          }
          this.askForKeyframe();
          return;
        }
        for (const id of [...this.complete.keys()]) if (before(id, key)) this.complete.delete(id);
        for (const id of [...this.partials.keys()]) if (before(id, key)) this.partials.delete(id);
        this.next = key;
        this.needKey = false;
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
      return;
    }
    const t = now();
    const missing = this.next;
    const partial = this.partials.get(missing);
    const overtaken = [...this.complete.values()].some((f) => t - f.lastAt > ORDER_WAIT_MS);
    const stale = partial ? t - partial.firstAt > FRAME_DEADLINE_MS : overtaken;
    if (!stale) return;
    // The frame the decoder needs next is gone: so is everything until a keyframe.
    let lost = 0;
    for (const id of [...this.partials.keys()]) if (!before(id, missing)) (this.partials.delete(id), lost++);
    lost += this.complete.size;
    this.complete.clear();
    post({ type: "lost", frames: Math.max(1, lost) });
    this.needKey = true;
    this.askForKeyframe();
  }

  private askForKeyframe(): void {
    const t = now();
    if (t - this.askedAt < KEYFRAME_RETRY_MS) return;
    this.askedAt = t;
    this.send(JSON.stringify({ t: "keyframe" }));
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
