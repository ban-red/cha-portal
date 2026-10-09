// Dev only: serves the harness MCP endpoint on the portal's Vite server, one per session:
//   POST /environments/<id>/session/mcp          the MCP endpoint (JSON-RPC)
//   GET  /environments/<id>/session/mcp/bridge   the session page's command stream (SSE)
//   POST /environments/<id>/session/mcp/bridge/result   the page's replies
// Off unless CHA_DEV_MCP=1: the paths then answer 404. Loopback only, and a browser page on
// another origin can't reach it (no CORS, and a request with an Origin must be this server's own).
// The commands run in the page's `window.__chaHarness`; see web/apps/portal/dev-mcp/rpc.ts.
import type { Plugin } from "vite";
import type { IncomingMessage, ServerResponse } from "node:http";

import { handleRpc, renderResult, TOOL_OPS, type PageReply, type ToolResult } from "./rpc";

const ROUTE = /^\/environments\/([A-Za-z0-9-]+)\/session\/mcp(\/bridge(\/result)?)?$/;
const LOOPBACK = new Set(["127.0.0.1", "::1", "::ffff:127.0.0.1"]);
/** How long a tool waits for the page; the harness's own waits are capped at 10 s. */
const CALL_TIMEOUT_MS = 30_000;
const MAX_BODY = 16 * 1024 * 1024;

interface Pending {
  resolve: (reply: PageReply) => void;
  timer: ReturnType<typeof setTimeout>;
}

interface Bridge {
  sse: ServerResponse | null;
  pending: Map<string, Pending>;
  seq: number;
}

function send(res: ServerResponse, status: number, body: string, type = "text/plain"): void {
  res.writeHead(status, { "content-type": type, "cache-control": "no-store" });
  res.end(body);
}

/**
 * Refuses a page on another origin, and (unless `lan`) anything that isn't from this machine.
 * `lan` is CHA_DEV_MCP_LAN=1: the endpoint has no login, so it is opt-in for a dev box on a LAN.
 */
function allowed(req: IncomingMessage, lan: boolean): boolean {
  if (!lan && !LOOPBACK.has(req.socket.remoteAddress ?? "")) return false;
  const origin = req.headers.origin;
  return origin === undefined || origin === `http://${req.headers.host}`;
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    size += (chunk as Buffer).length;
    if (size > MAX_BODY) throw new Error("body too large");
    chunks.push(chunk as Buffer);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

export function devMcp(enabled: boolean, lan = false): Plugin {
  const bridges = new Map<string, Bridge>();

  const bridgeFor = (env: string): Bridge => {
    let b = bridges.get(env);
    if (!b) {
      b = { sse: null, pending: new Map(), seq: 0 };
      bridges.set(env, b);
    }
    return b;
  };

  /** One tool call: a command down the page's stream, the page's reply back. */
  const callTool = (env: string, host: string | undefined, name: string, args: Record<string, unknown>): Promise<ToolResult> => {
    const b = bridgeFor(env);
    if (!b.sse) {
      return Promise.resolve({
        isError: true,
        content: [
          {
            type: "text",
            text: `No session page is attached for environment ${env}. Open http://${host}/environments/${env}/session, sign in, and click the picture once.`,
          },
        ],
      });
    }
    const id = String(++b.seq);
    const reply = new Promise<PageReply>((resolve) => {
      const timer = setTimeout(() => {
        b.pending.delete(id);
        resolve({ ok: false, error: "the session page didn't answer in time" });
      }, CALL_TIMEOUT_MS);
      b.pending.set(id, { resolve, timer });
    });
    b.sse.write(`data: ${JSON.stringify({ id, op: TOOL_OPS[name], args })}\n\n`);
    return reply.then((r) => renderResult(name, r));
  };

  return {
    name: "cha-dev-mcp",
    apply: "serve",
    configureServer(server) {
      // Ahead of Vite's SPA fallback, so these paths never serve index.html.
      server.middlewares.use(async (req, res, next) => {
        const m = ROUTE.exec((req.url ?? "").split("?")[0] ?? "");
        if (!m) return next();
        if (!enabled) return send(res, 404, "not found");
        if (!allowed(req, lan)) return send(res, 403, "the dev harness is loopback-only (CHA_DEV_MCP_LAN=1 allows the LAN)");

        const env = m[1]!;
        const sub = m[2] ?? "";
        const host = req.headers.host;

        try {
          if (sub === "") {
            if (req.method !== "POST") return send(res, 405, "POST only");
            if (!req.headers["content-type"]?.startsWith("application/json")) return send(res, 415, "send application/json");
            const body = await readJson(req);
            const reply = await handleRpc(body, (name, args) => callTool(env, host, name, args));
            if (reply === null) return send(res, 202, "");
            return send(res, 200, JSON.stringify(reply), "application/json");
          }

          if (sub === "/bridge") {
            if (req.method !== "GET") return send(res, 405, "GET only");
            const b = bridgeFor(env);
            res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
            res.write(": attached\n\n");
            // A newer page for this environment takes over the stream.
            b.sse?.end();
            b.sse = res;
            const ping = setInterval(() => res.write(": ping\n\n"), 15_000);
            req.on("close", () => {
              clearInterval(ping);
              if (b.sse === res) b.sse = null;
            });
            return;
          }

          // /bridge/result
          if (req.method !== "POST") return send(res, 405, "POST only");
          const body = (await readJson(req)) as { id?: unknown; ok?: unknown; result?: unknown; error?: unknown };
          const b = bridgeFor(env);
          const p = typeof body.id === "string" || typeof body.id === "number" ? b.pending.get(String(body.id)) : undefined;
          if (p) {
            clearTimeout(p.timer);
            b.pending.delete(String(body.id));
            p.resolve({
              ok: body.ok === true,
              result: body.result,
              error: typeof body.error === "string" ? body.error : undefined,
            });
          }
          return send(res, 204, "");
        } catch (e) {
          return send(res, 400, e instanceof Error ? e.message : "bad request");
        }
      });
    },
  };
}
