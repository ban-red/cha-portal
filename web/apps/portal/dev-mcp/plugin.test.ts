import { expect, test } from "bun:test";

import { devMcp } from "./plugin";

type Handler = (req: any, res: any, next: () => void) => unknown;

/** A Vite-like server: captures the middleware the plugin registers. */
function mount(enabled: boolean, lan = false): Handler {
  let mw: Handler | undefined;
  const plugin = devMcp(enabled, lan) as any;
  plugin.configureServer({ middlewares: { use: (fn: Handler) => (mw = fn) } });
  return (req, res, next) => mw!(req, res, next);
}

function fakeReq(method: string, url: string, body?: unknown, opts: { remote?: string; origin?: string; type?: string } = {}) {
  const listeners: Record<string, (() => void)[]> = {};
  const req: any = {
    method,
    url,
    headers: {
      host: "localhost:7678",
      "content-type": opts.type ?? "application/json",
      ...(opts.origin ? { origin: opts.origin } : {}),
    },
    socket: { remoteAddress: opts.remote ?? "127.0.0.1" },
    on: (ev: string, cb: () => void) => ((listeners[ev] ??= []).push(cb), req),
    close: () => (listeners.close ?? []).forEach((cb) => cb()),
    async *[Symbol.asyncIterator]() {
      if (body !== undefined) yield Buffer.from(typeof body === "string" ? body : JSON.stringify(body));
    },
  };
  return req;
}

function fakeRes() {
  const res: any = {
    statusCode: 200,
    status: undefined as number | undefined,
    body: "",
    written: [] as string[],
    writeHead(status: number) {
      res.status = status;
      return res;
    },
    write(chunk: string) {
      res.written.push(chunk);
      return true;
    },
    end(chunk?: string) {
      if (chunk) res.body += chunk;
      res.ended = true;
      return res;
    },
  };
  return res;
}

const ENV = "01a11ecd-4db7-753b-89ca-0f0c60f2db80";
const MCP = `/environments/${ENV}/session/mcp`;

test("off by default: the endpoint answers 404 and never reaches the SPA", async () => {
  const mw = mount(false);
  const res = fakeRes();
  let nexted = false;
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }), res, () => (nexted = true));
  expect(res.status).toBe(404);
  expect(nexted).toBe(false);
});

test("other paths pass through untouched", async () => {
  const mw = mount(true);
  let nexted = false;
  await mw(fakeReq("GET", `/environments/${ENV}/session`), fakeRes(), () => (nexted = true));
  expect(nexted).toBe(true);
});

test("loopback only: a request from another machine is refused", async () => {
  const mw = mount(true);
  const res = fakeRes();
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }, { remote: "192.168.1.20" }), res, () => {});
  expect(res.status).toBe(403);
});

test("CHA_DEV_MCP_LAN lets another machine in, but a page on another origin still can't", async () => {
  const lan = mount(true, true);
  const ok = fakeRes();
  await lan(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }, { remote: "192.168.1.20" }), ok, () => {});
  expect(ok.status).toBe(200);

  const cross = fakeRes();
  await lan(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }, { remote: "192.168.1.20", origin: "https://evil.example" }), cross, () => {});
  expect(cross.status).toBe(403);
});

test("a page on another origin is refused", async () => {
  const mw = mount(true);
  const res = fakeRes();
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }, { origin: "https://evil.example" }), res, () => {});
  expect(res.status).toBe(403);
});

test("the MCP endpoint wants JSON, and lists the tools", async () => {
  const mw = mount(true);
  const bad = fakeRes();
  await mw(fakeReq("POST", MCP, "{}", { type: "text/plain" }), bad, () => {});
  expect(bad.status).toBe(415);

  const res = fakeRes();
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/list" }), res, () => {});
  expect(res.status).toBe(200);
  expect(JSON.parse(res.body).result.tools.length).toBe(10);
});

test("a notification gets 202 with no body", async () => {
  const mw = mount(true);
  const res = fakeRes();
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", method: "notifications/initialized" }), res, () => {});
  expect(res.status).toBe(202);
  expect(res.body).toBe("");
});

test("with no page attached, a tool says how to attach one", async () => {
  const mw = mount(true);
  const res = fakeRes();
  await mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "status" } }), res, () => {});
  const reply = JSON.parse(res.body).result;
  expect(reply.isError).toBe(true);
  expect(reply.content[0].text).toContain(`http://localhost:7678/environments/${ENV}/session`);
});

test("a command goes down the page's stream and its reply comes back as the tool result", async () => {
  const mw = mount(true);

  // The session page attaches its bridge.
  const stream = fakeRes();
  const bridge = fakeReq("GET", `${MCP}/bridge`);
  await mw(bridge, stream, () => {});
  expect(stream.status).toBe(200);

  // An MCP client asks for the status.
  const res = fakeRes();
  const call = mw(fakeReq("POST", MCP, { jsonrpc: "2.0", id: 9, method: "tools/call", params: { name: "status" } }), res, () => {});

  // The command arrived on the stream; the page answers it.
  await new Promise((r) => setTimeout(r, 10));
  const line = stream.written.find((w: string) => w.startsWith("data: "));
  const cmd = JSON.parse(line!.slice(6));
  expect(cmd.op).toBe("status");

  const posted = fakeRes();
  await mw(fakeReq("POST", `${MCP}/bridge/result`, { id: cmd.id, ok: true, result: { harness: true } }), posted, () => {});
  expect(posted.status).toBe(204);

  await call;
  const reply = JSON.parse(res.body).result;
  expect(reply.isError).toBeUndefined();
  expect(JSON.parse(reply.content[0].text)).toEqual({ harness: true });

  bridge.close();
});

test("a failed page reply comes back as a tool error", async () => {
  const mw = mount(true);
  const stream = fakeRes();
  const bridge = fakeReq("GET", `${MCP}/bridge`);
  await mw(bridge, stream, () => {});

  const res = fakeRes();
  const call = mw(
    fakeReq("POST", MCP, { jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "view_frame", arguments: { width: 128 } } }),
    res,
    () => {},
  );
  await new Promise((r) => setTimeout(r, 10));
  const cmd = JSON.parse(stream.written.find((w: string) => w.startsWith("data: "))!.slice(6));
  expect(cmd.op).toBe("view");
  expect(cmd.args).toEqual({ width: 128 });

  await mw(fakeReq("POST", `${MCP}/bridge/result`, { id: cmd.id, ok: false, error: "no picture yet" }), fakeRes(), () => {});
  await call;
  const reply = JSON.parse(res.body).result;
  expect(reply.isError).toBe(true);
  expect(reply.content[0].text).toBe("no picture yet");

  bridge.close();
});
