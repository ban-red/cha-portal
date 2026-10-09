import { expect, test } from "bun:test";

import { handleRpc, renderResult, TOOLS, type Call, type ToolResult } from "./rpc";

const noCall: Call = async () => {
  throw new Error("no tool should run");
};

const echo: Call = async (name, args) => ({ content: [{ type: "text", text: JSON.stringify({ name, args }) }] });

test("initialize names the server and the tools capability", async () => {
  const r = (await handleRpc({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} }, noCall)) as any;
  expect(r.id).toBe(1);
  expect(r.result.capabilities).toEqual({ tools: {} });
  expect(r.result.serverInfo.name).toBe("cha-dev-harness");
});

test("notifications get no reply", async () => {
  expect(await handleRpc({ jsonrpc: "2.0", method: "notifications/initialized" }, noCall)).toBeNull();
});

test("tools/list offers the harness tools", async () => {
  const r = (await handleRpc({ jsonrpc: "2.0", id: 2, method: "tools/list" }, noCall)) as any;
  expect(r.result.tools.map((t: { name: string }) => t.name)).toEqual(TOOLS.map((t) => t.name));
  expect(r.result.tools.map((t: { name: string }) => t.name)).toEqual([
    "view_frame",
    "input",
    "pad_stream",
    "sense_frame",
    "wait_for_change",
    "wait_for",
    "servo",
    "stop",
    "status",
    "guide",
  ]);
});

test("a tool call runs with its name and arguments", async () => {
  const r = (await handleRpc(
    { jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "input", arguments: { action: "hold", code: "KeyW", ms: 800 } } },
    echo,
  )) as any;
  expect(JSON.parse(r.result.content[0].text)).toEqual({ name: "input", args: { action: "hold", code: "KeyW", ms: 800 } });
});

test("an unknown method or tool is a protocol error", async () => {
  const bad = (await handleRpc({ jsonrpc: "2.0", id: 4, method: "resources/list" }, noCall)) as any;
  expect(bad.error.code).toBe(-32601);
  const unknown = (await handleRpc({ jsonrpc: "2.0", id: 5, method: "tools/call", params: { name: "press_everything" } }, noCall)) as any;
  expect(unknown.error.code).toBe(-32602);
});

test("an input action outside the list is a tool error, and never reaches the page", async () => {
  const r = (await handleRpc(
    { jsonrpc: "2.0", id: 6, method: "tools/call", params: { name: "input", arguments: { action: "exec" } } },
    noCall,
  )) as any;
  expect(r.result.isError).toBe(true);
});

test("an action sequence passes validation and reaches the page", async () => {
  const r = (await handleRpc(
    {
      jsonrpc: "2.0",
      id: 7,
      method: "tools/call",
      params: { name: "input", arguments: { actions: [{ action: "hold", code: "KeyW", ms: 600 }, { action: "look", dx: 90 }] } },
    },
    echo,
  )) as any;
  expect(r.result.isError).toBeUndefined();
  expect(JSON.parse(r.result.content[0].text).args.actions).toHaveLength(2);
});

test("a sequence with a bad step, or past the ms budget, is a tool error", async () => {
  const badStep = (await handleRpc(
    { jsonrpc: "2.0", id: 8, method: "tools/call", params: { name: "input", arguments: { actions: [{ action: "exec" }] } } },
    noCall,
  )) as any;
  expect(badStep.result.isError).toBe(true);

  const over = (await handleRpc(
    { jsonrpc: "2.0", id: 9, method: "tools/call", params: { name: "input", arguments: { actions: [{ action: "hold", code: "KeyW", ms: 11000 }, { action: "wait", ms: 11000 }] } } },
    noCall,
  )) as any;
  expect(over.result.isError).toBe(true);
});

test("wait_for and servo want a match with a channel", async () => {
  for (const name of ["wait_for", "servo"]) {
    const r = (await handleRpc(
      { jsonrpc: "2.0", id: 10, method: "tools/call", params: { name, arguments: { match: { channel: "x" } } } },
      noCall,
    )) as any;
    expect(r.result.isError).toBe(true);
    expect(r.result.content[0].text).toContain("channel");
  }
});

test("pad_stream wants a list of steps", async () => {
  const bad = (await handleRpc(
    { jsonrpc: "2.0", id: 12, method: "tools/call", params: { name: "pad_stream", arguments: { steps: "a" } } },
    noCall,
  )) as any;
  expect(bad.result.isError).toBe(true);

  const ok = (await handleRpc(
    { jsonrpc: "2.0", id: 13, method: "tools/call", params: { name: "pad_stream", arguments: { steps: [{ buttons: { b: 1 } }] } } },
    echo,
  )) as any;
  expect(ok.result.isError).toBeUndefined();
  expect(JSON.parse(ok.result.content[0].text).args.steps).toHaveLength(1);
});

test("a non-object message is rejected", async () => {
  const r = (await handleRpc("hello", noCall)) as any;
  expect(r.error.code).toBe(-32600);
});

test("the guide is self-served, with no page and no session", async () => {
  const r = (await handleRpc(
    { jsonrpc: "2.0", id: 11, method: "tools/call", params: { name: "guide", arguments: {} } },
    noCall,
  )) as any;
  expect(r.result.isError).toBeUndefined();
  const text = r.result.content[0].text as string;
  expect(text).toContain("status.locked");
  expect(text).toContain("servo");
  expect(text).toContain("wait_for");
});

test("a frame reply becomes an image plus its size", () => {
  const out = renderResult("view_frame", { ok: true, result: { w: 256, h: 144, png: "AAAA" } });
  expect(out.content[0]).toEqual({ type: "image", data: "AAAA", mimeType: "image/png" });
  expect(out.content[1]).toEqual({ type: "text", text: "256x144 picture" });
});

test("a failed page reply becomes a tool error with the page's message", () => {
  const out: ToolResult = renderResult("input", { ok: false, error: "no picture yet" });
  expect(out.isError).toBe(true);
  expect(out.content[0]).toEqual({ type: "text", text: "no picture yet" });
});
