#!/usr/bin/env bun
// Setup and teardown for the pad-latency probe (docs/benchmarks/README.md has the whole procedure).
//
//   bun scripts/pad-latency.ts create [node]   launch a test-pattern environment as the dev user
//                                               (needs `bun run dev`); prints its session URL
//   bun scripts/pad-latency.ts delete <id>     destroy it
//
// The measurement itself runs in the session page (the harness is dev-only):
//   await window.__chaHarness.padProbe({ count: 100 })
// Needs the dev portal's API on CHA_PORTAL (default http://localhost:7677) and its SPA on :7678.
const api = process.env.CHA_PORTAL ?? "http://localhost:7677";
const spa = process.env.CHA_SPA ?? "http://localhost:7678";

let cookie = "";
async function call(method: string, path: string, body?: unknown): Promise<any> {
  const res = await fetch(`${api}/api${path}`, {
    method,
    headers: { "content-type": "application/json", ...(cookie ? { cookie } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const set = res.headers.get("set-cookie");
  if (set) cookie = set.split(";")[0]!;
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${text}`);
  return text ? JSON.parse(text) : null;
}

const [cmd, arg] = process.argv.slice(2);
await call("POST", "/auth/dev-login", {});
if (cmd === "create") {
  const nodes = (await call("GET", "/nodes")) as { id: string; name: string; online: boolean }[];
  const node = nodes.find((n) => n.online && (!arg || n.name === arg));
  if (!node) throw new Error(`no online node${arg ? ` named ${arg}` : ""}`);
  const env = await call("POST", "/environments", { templateId: "test-pattern", node: node.id });
  for (let i = 0; i < 90; i++) {
    const e = await call("GET", `/environments/${env.id}`);
    if (e.state === "running") {
      console.log(`${spa}/environments/${e.id}/session`);
      console.log(`id ${e.id} on ${e.nodeName}`);
      process.exit(0);
    }
    if (e.state === "failed") throw new Error(`environment failed: ${e.detail ?? ""}`);
    await Bun.sleep(1000);
  }
  throw new Error("timed out waiting for the environment to run");
} else if (cmd === "delete" && arg) {
  await call("DELETE", `/environments/${arg}`);
  console.log(`deleted ${arg}`);
} else {
  console.error("usage: bun scripts/pad-latency.ts create [node] | delete <id>");
  process.exit(2);
}
