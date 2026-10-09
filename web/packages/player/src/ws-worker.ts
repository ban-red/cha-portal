// The WebSocket session (`cha-stream/1` over one WebSocket, ADR 0022), off the
// main thread: opens the first URL that answers, feeds the binary messages
// (datagrams, in order, no FEC) to the shared reassembly (reassembly.ts), and
// relays the text messages (the control stream's JSON lines) both ways. Same
// messages to and from the page as wt-worker.ts.

import { Reassembler, post, setClock, type ToWorker } from "./reassembly";
import { resolveWsUrl } from "./ws-url";

const CONNECT_TIMEOUT_MS = 8000;

let session: Session | null = null;

self.onmessage = (e: MessageEvent<ToWorker>) => {
  const msg = e.data;
  if (msg.type === "start") {
    start(msg.urls).catch((err: unknown) => {
      const reason = `WebSocket failed to open: ${err instanceof Error ? err.message : String(err)}`;
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

async function start(urls: string[]): Promise<void> {
  for (const raw of urls) {
    const url = resolveWsUrl(raw, self.location.href);
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    const opened = await new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => resolve(false), CONNECT_TIMEOUT_MS);
      ws.onopen = () => (clearTimeout(timer), resolve(true));
      ws.onclose = ws.onerror = () => (clearTimeout(timer), resolve(false));
    });
    if (!opened) {
      ws.onopen = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        // Never opened.
      }
      continue;
    }
    session = new Session(ws);
    session.open(raw);
    return;
  }
  post({ type: "closed", reason: "none of the streamer's addresses answered over WebSocket" });
}

class Session {
  private readonly rx = new Reassembler((line) => this.send(line));
  private closed = false;

  constructor(private readonly ws: WebSocket) {}

  open(url: string): void {
    this.ws.onmessage = (e: MessageEvent<string | ArrayBuffer>) => {
      if (typeof e.data === "string") {
        for (const line of e.data.split("\n")) if (line.trim()) post({ type: "line", line: line.trim() });
      } else {
        this.rx.datagram(new Uint8Array(e.data));
      }
    };
    this.ws.onclose = (e) => this.close(e.reason || "the streamer closed the session");
    this.ws.onerror = () => this.close("the WebSocket failed");
    post({ type: "ready", url });
    this.rx.start();
  }

  send(line: string): void {
    if (this.ws.readyState !== WebSocket.OPEN) return;
    try {
      this.ws.send(line.endsWith("\n") ? line.slice(0, -1) : line);
    } catch {
      this.close("control write failed");
    }
  }

  close(reason: string): void {
    if (this.closed) return;
    this.closed = true;
    this.rx.stop();
    try {
      this.ws.close();
    } catch {
      // Already closed.
    }
    post({ type: "closed", reason });
  }
}
