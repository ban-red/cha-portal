// The WebTransport session (`cha-stream/1`), off the main thread: connects to
// the first of the streamer's URLs that answers, feeds datagrams to the shared
// reassembly (reassembly.ts), and relays the control stream's JSON lines both
// ways.

import { Reassembler, post, setClock, type ToWorker } from "./reassembly";

const CONNECT_TIMEOUT_MS = 2500;

let session: Session | null = null;

self.onmessage = (e: MessageEvent<ToWorker>) => {
  const msg = e.data;
  if (msg.type === "start") {
    // Any failure to open (a throw from `new WebTransport`, a stream that won't open) reaches the page as `closed`.
    start(msg.urls, msg.certHash).catch((err: unknown) => {
      const reason = `WebTransport failed to open: ${err instanceof Error ? err.message : String(err)}`;
      if (session) session.close(reason);
      else post({ type: "closed", reason });
    });
  } else if (msg.type === "send") {
    session?.send(msg.line);
  } else if (msg.type === "clock") {
    setClock(msg.offsetMs);
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
  private readonly rx = new Reassembler((line) => this.send(line));
  private closed = false;

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
    this.rx.start();
    void this.readControl(ctl.readable);
    await this.readDatagrams();
  }

  send(line: string): void {
    this.writer?.write(this.encoder.encode(line.endsWith("\n") ? line : line + "\n")).catch(() => this.close("control write failed"));
  }

  close(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    this.rx.stop();
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
      if (done || !value) return this.close("control stream ended");
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
      if (done || !value) return this.close("datagrams ended");
      this.rx.datagram(value as Uint8Array);
    }
  }
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
