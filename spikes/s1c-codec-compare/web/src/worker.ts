// WebTransport receiver for S1c: reassembles whole frames (cha-stream/1 framing)
// and hands them to the page with absolute, clock-synced timestamps.

export interface StartMsg {
  type: "start";
  host: string;
  wtPort: number;
  certHashHex: string;
  stream: string;
  fps: number;
  secs: number;
  dgram: number;
}

export interface FrameMsg {
  type: "frame";
  id: number;
  key: boolean;
  /** Absolute ms (timeOrigin + now). `sentAt` is null until the clock is synced. */
  sentAt: number | null;
  firstAt: number;
  lastAt: number;
  data: ArrayBuffer;
}

export type WorkerMsg =
  | { type: "hello"; hello: Record<string, unknown> }
  | FrameMsg
  | { type: "dropped"; id: number; received: number; total: number }
  | { type: "server"; stats: Record<string, unknown> }
  | { type: "done"; reason: string; minRttMs: number | null }
  | { type: "error"; message: string };

const HEADER_LEN = 16;
const FRAME_TIMEOUT_MS = 250;
const post = (msg: WorkerMsg, transfer: Transferable[] = []) => self.postMessage(msg, { transfer });
const abs = (local: number) => performance.timeOrigin + local;

interface Partial {
  total: number;
  received: number;
  parts: (Uint8Array | undefined)[];
  bytes: number;
  key: boolean;
  sendTsUs: number;
  firstAt: number;
}

/** Keeps the lowest-RTT ping sample; maps the sender's clock onto ours. */
class ClockSync {
  private best: { rtt: number; offset: number } | null = null;
  onPong(sentLocal: number, serverUs: number, nowLocal: number): void {
    const rtt = nowLocal - sentLocal;
    if (!this.best || rtt < this.best.rtt) this.best = { rtt, offset: serverUs / 1000 - (sentLocal + nowLocal) / 2 };
  }
  get minRtt(): number | null {
    return this.best?.rtt ?? null;
  }
  toAbs(sendTsUs: number): number | null {
    return this.best ? abs(sendTsUs / 1000 - this.best.offset) : null;
  }
}

self.onmessage = (e: MessageEvent<StartMsg>) => {
  if (e.data.type === "start") run(e.data).catch((err: unknown) => post({ type: "error", message: String(err) }));
};

async function run(start: StartMsg): Promise<void> {
  const host = start.host.includes(":") ? `[${start.host}]` : start.host;
  const query = `name=${encodeURIComponent(start.stream)}&fps=${start.fps}&secs=${start.secs}&dgram=${start.dgram}`;
  const wt = new WebTransport(`https://${host}:${start.wtPort}/stream?${query}`, {
    serverCertificateHashes: [{ algorithm: "sha-256", value: hexToBytes(start.certHashHex) }],
    requireUnreliable: true,
  });
  await wt.ready;
  try {
    (wt.datagrams as { incomingHighWaterMark?: number }).incomingHighWaterMark = 4096;
  } catch {
    // Read-only in some engines.
  }

  const clock = new ClockSync();
  const frames = new Map<number, Partial>();
  const finished = new Set<number>();
  let maxId = -1;
  let ended = false;
  const end = (reason: string) => {
    if (ended) return;
    ended = true;
    clearInterval(pinger);
    clearInterval(expirer);
    post({ type: "done", reason, minRttMs: clock.minRtt });
    try {
      wt.close();
    } catch {
      // Already closed.
    }
  };

  const ctl = await wt.createBidirectionalStream();
  const writer = ctl.writable.getWriter();
  const encoder = new TextEncoder();
  const ping = () => void writer.write(encoder.encode(JSON.stringify({ t: "ping", c: performance.now() }) + "\n")).catch(() => {});
  ping(); // also makes the control stream visible to the server
  const pinger = setInterval(ping, 200);

  void (async () => {
    const reader = ctl.readable.getReader();
    const text = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += text.decode(value, { stream: true });
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line) continue;
        const msg = JSON.parse(line) as { t: string; c?: number; s_us?: number };
        if (msg.t === "pong") clock.onPong(msg.c!, msg.s_us!, performance.now());
        else if (msg.t === "hello") post({ type: "hello", hello: msg });
        else if (msg.t === "stats") post({ type: "server", stats: msg });
        else if (msg.t === "done") {
          post({ type: "server", stats: msg });
          setTimeout(() => end("done"), 400);
        }
      }
    }
  })().catch(() => {});
  const expirer = setInterval(() => {
    const now = performance.now();
    for (const [id, f] of frames) {
      if (now - (f.firstAt - performance.timeOrigin) < FRAME_TIMEOUT_MS) continue;
      frames.delete(id);
      finished.add(id);
      post({ type: "dropped", id, received: f.received, total: f.total });
    }
  }, 50);

  wt.closed.then(
    () => end("closed"),
    (err: unknown) => end(`closed: ${String(err)}`),
  );

  const reader = wt.datagrams.readable.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    const d = value as Uint8Array;
    const now = abs(performance.now());
    if (d.byteLength < HEADER_LEN) continue;
    const v = new DataView(d.buffer, d.byteOffset, HEADER_LEN);
    const id = v.getUint32(4, true);
    const index = v.getUint16(8, true);
    const total = v.getUint16(10, true);
    if (total === 0 || index >= total || finished.has(id)) continue;
    if (id > maxId) maxId = id;
    let f = frames.get(id);
    if (!f) {
      f = {
        total,
        received: 0,
        parts: new Array(total),
        bytes: 0,
        key: (v.getUint8(1) & 1) === 1,
        sendTsUs: v.getUint32(12, true),
        firstAt: now,
      };
      frames.set(id, f);
    }
    if (f.parts[index]) continue;
    const payload = d.subarray(HEADER_LEN);
    f.parts[index] = payload.slice();
    f.bytes += payload.byteLength;
    f.received++;
    if (f.received < f.total) continue;

    frames.delete(id);
    finished.add(id);
    if (finished.size > 2048) for (const old of finished) if (old < maxId - 1024) finished.delete(old);
    const data = new Uint8Array(f.bytes);
    let at = 0;
    for (const part of f.parts) {
      data.set(part!, at);
      at += part!.byteLength;
    }
    post(
      { type: "frame", id, key: f.key, sentAt: clock.toAbs(f.sendTsUs), firstAt: f.firstAt, lastAt: now, data: data.buffer },
      [data.buffer],
    );
  }
  end("datagrams closed");
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
