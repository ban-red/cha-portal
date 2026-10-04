// WebTransport receiver. Runs in a dedicated Worker so the datagram read loop
// never competes with page rendering.

import { trafficQuery, type TrafficConfig } from "./proto";
import { Run, type RunProgress, type RunResult } from "./run";

export interface WtStart {
  type: "start";
  host: string;
  wtPort: number;
  certHashHex: string;
  config: TrafficConfig;
  /** Read datagrams with a BYOB reader into a reused buffer. */
  byob: boolean;
  /** Value for `datagrams.incomingHighWaterMark` (datagrams queued before drops). */
  highWaterMark: number;
}

export type WtEvent =
  | { type: "progress"; progress: RunProgress }
  | { type: "result"; result: RunResult }
  | { type: "error"; message: string };

const post = (msg: WtEvent) => self.postMessage(msg);

self.onmessage = (e: MessageEvent<WtStart>) => {
  if (e.data.type !== "start") return;
  run(e.data).catch((err: unknown) => post({ type: "error", message: String(err) }));
};

async function run(start: WtStart): Promise<void> {
  const host = start.host.includes(":") ? `[${start.host}]` : start.host;
  const url = `https://${host}:${start.wtPort}/s1?${trafficQuery(start.config)}`;
  const wt = new WebTransport(url, {
    serverCertificateHashes: [{ algorithm: "sha-256", value: hexToBytes(start.certHashHex) }],
    requireUnreliable: true,
  });
  await wt.ready;
  try {
    (wt.datagrams as { incomingHighWaterMark?: number }).incomingHighWaterMark = start.highWaterMark;
  } catch {
    // Older engines expose it read-only; keep the default.
  }

  const ctl = await wt.createBidirectionalStream();
  const writer = ctl.writable.getWriter();
  const encoder = new TextEncoder();
  const run = new Run(
    "webtransport",
    start.config,
    (line) => void writer.write(encoder.encode(line)).catch(() => {}),
    (progress) => post({ type: "progress", progress }),
  );

  void (async () => {
    const reader = ctl.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      run.onControl(value);
    }
  })().catch(() => {});

  wt.closed.then(
    () => run.finish("closed"),
    (err: unknown) => run.finish(`closed: ${String(err)}`),
  );

  run.start();
  void readDatagrams(wt, run, start.byob).catch(() => {});

  const result = await run.result;
  post({ type: "result", result });
  try {
    wt.close();
  } catch {
    // Already closed by the server.
  }
}

async function readDatagrams(wt: WebTransport, run: Run, byob: boolean): Promise<void> {
  if (byob) {
    const reader = wt.datagrams.readable.getReader({ mode: "byob" });
    let buf = new ArrayBuffer(65536);
    for (;;) {
      const { value, done } = await reader.read(new Uint8Array(buf));
      if (done || !value) break;
      run.onDatagram(value);
      buf = value.buffer as ArrayBuffer;
    }
    return;
  }
  const reader = wt.datagrams.readable.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    run.onDatagram(value as Uint8Array);
  }
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
