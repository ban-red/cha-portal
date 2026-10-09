// The MCP side of the dev harness bridge: JSON-RPC 2.0 over the streamable-HTTP transport
// (plain JSON replies, no sessions). Pure, so the dev server's plugin and the tests share it.
// The tools run in the session page through `window.__chaHarness` (see
// web/packages/player/src/dev-bridge.ts); this file only names them and shapes the replies.

export const PROTOCOL = "2025-03-26";

export const INPUT_ACTIONS = [
  "hold", "tap", "chord", "type", "look", "move_to", "click", "wait", "seed",
  "pad", "pad_stick", "pad_tap", "pad_pull",
] as const;

export type ContentItem = { type: "text"; text: string } | { type: "image"; data: string; mimeType: string };

export interface ToolResult {
  content: ContentItem[];
  isError?: boolean;
}

/** What the session page sends back for one command. */
export interface PageReply {
  ok: boolean;
  result?: unknown;
  error?: string;
}

const region = {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" }, w: { type: "number" }, h: { type: "number" } },
  required: ["x", "y", "w", "h"],
  additionalProperties: false,
};

const match = {
  type: "object",
  properties: {
    channel: { enum: ["r", "g", "b"] },
    min: { type: "integer", minimum: 0, maximum: 255 },
    max: { type: "integer", minimum: 0, maximum: 255 },
  },
  required: ["channel"],
  additionalProperties: false,
};

export const TOOLS = [
  {
    name: "view_frame",
    description:
      "Look at the session's current picture as a PNG. width is the output width in pixels (64-1024, default 256). region crops to fractions of the picture, each 0..1 (x, y from the top left). Prefer sense_frame for control: this costs an image read.",
    inputSchema: {
      type: "object",
      properties: { width: { type: "integer", minimum: 64, maximum: 1024 }, region },
      additionalProperties: false,
    },
  },
  {
    name: "input",
    description:
      "Send input to the session as a person's devices would. hold/tap/chord take KeyboardEvent codes such as KeyW or Space; type takes text (US layout). look turns the camera and works only after the picture has been clicked once (pointer lock). move_to takes fractions of the picture. ms values are milliseconds, up to 10000. " +
      "Or pass actions: an array of such steps (each with its own action and args) run in order, total ms up to 20000 — one round trip for a whole maneuver. " +
      "sense: true adds a colour grid of the picture taken settleMs (default 80) after the action, so acting and observing share a call. " +
      "wait: false starts a hold, chord or pad stick and returns at once (the input stays down across calls; use stop to let go). " +
      "Controller, as the session's own pad (a real pad on this page would fight it): pad holds an exact state for ms, then lets go — buttons maps a, b, x, y, lb, rb, lt, rt (0..1, triggers analog), back, start, ls, rs, up, down, left, right, guide to 0..1; axes maps lx, ly, rx, ry to -1..1. " +
      "pad_stick moves sticks like a thumb (rise, tremor, spring release; style flick for camera swings), pad_tap mashes the button named in code count times, pad_pull ramps trigger lt/rt to depth (0..1) and eases off.",
    inputSchema: {
      type: "object",
      properties: {
        action: { enum: [...INPUT_ACTIONS] },
        code: { type: "string" },
        codes: { type: "array", items: { type: "string" } },
        text: { type: "string" },
        buttons: { type: "object", additionalProperties: { type: "number", minimum: 0, maximum: 1 } },
        axes: { type: "object", additionalProperties: { type: "number", minimum: -1, maximum: 1 } },
        style: { enum: ["tilt", "flick"] },
        count: { type: "integer", minimum: 1, maximum: 32 },
        trigger: { enum: ["lt", "rt"] },
        depth: { type: "number", minimum: 0, maximum: 1 },
        ms: { type: "number", minimum: 0, maximum: 10000 },
        dx: { type: "number" },
        dy: { type: "number" },
        fx: { type: "number", minimum: 0, maximum: 1 },
        fy: { type: "number", minimum: 0, maximum: 1 },
        button: { type: "integer", minimum: 0, maximum: 4 },
        seed: { type: "integer" },
        actions: { type: "array", items: { type: "object" }, maxItems: 32 },
        sense: { type: "boolean" },
        settleMs: { type: "integer", minimum: 0, maximum: 1000 },
        cols: { type: "integer", minimum: 2, maximum: 32 },
        wait: { type: "boolean" },
      },
      additionalProperties: false,
    },
  },
  {
    name: "pad_stream",
    description:
      "Play a short run of pad states at game frame rate (frame_ms, default 16.7: 60 a second) for a driver that predicts ahead. Each step is a full state: buttons (a, b, x, y, lb, rb, lt, rt, back, start, ls, rs, up, down, left, right, guide: 0..1) and axes (lx, ly, rx, ry: -1..1); anything a step leaves out is released. A newer pad_stream replaces the one playing, so chunks can be streamed back to back with no neutral gap. The last state is held for hold_ms (default 250) and then released, so a driver that dies does not leave a button down. wait: true waits for the run to finish; by default it returns at once. stop cancels it.",
    inputSchema: {
      type: "object",
      required: ["steps"],
      properties: {
        steps: {
          type: "array",
          maxItems: 120,
          items: {
            type: "object",
            properties: {
              buttons: { type: "object", additionalProperties: { type: "number", minimum: 0, maximum: 1 } },
              axes: { type: "object", additionalProperties: { type: "number", minimum: -1, maximum: 1 } },
            },
            additionalProperties: false,
          },
        },
        frame_ms: { type: "number", minimum: 8, maximum: 100 },
        hold_ms: { type: "number", minimum: 0, maximum: 2000 },
        wait: { type: "boolean" },
      },
      additionalProperties: false,
    },
  },
  {
    name: "sense_frame",
    description:
      "A colour grid of the current picture: cols cells across (2-32, default 8), rows to the aspect, row-major — a few hundred bytes instead of a screenshot. Read this on control turns; view_frame only when you must genuinely see.",
    inputSchema: {
      type: "object",
      properties: { cols: { type: "integer", minimum: 2, maximum: 32 } },
      additionalProperties: false,
    },
  },
  {
    name: "wait_for_change",
    description: "Wait until the picture differs from now, for up to timeoutMs. Returns whether it changed.",
    inputSchema: {
      type: "object",
      properties: { timeoutMs: { type: "integer", minimum: 100, maximum: 20000 } },
      additionalProperties: false,
    },
  },
  {
    name: "wait_for",
    description:
      "One round trip, closed in the page: wait until fraction (default 0.5) of a region's pixels match a channel range — e.g. the blue door filling the right third after a look. Checks on presented frames; returns matched and the fraction now standing.",
    inputSchema: {
      type: "object",
      properties: {
        match,
        region,
        fraction: { type: "number", minimum: 0.01, maximum: 1 },
        timeoutMs: { type: "integer", minimum: 100, maximum: 20000 },
        every: { type: "integer", minimum: 1, maximum: 10 },
        width: { type: "integer", minimum: 64, maximum: 256 },
      },
      required: ["match"],
      additionalProperties: false,
    },
  },
  {
    name: "servo",
    description:
      "Aim servo, closed in the page at frame rate: keep a colour target centred with small camera corrections until it is lost, ms passes (default 4000), or stop is called. You set the policy; the page holds the aim. Needs pointer lock (click the picture once first). wait: false starts it and returns at once. Returns where the target last sat.",
    inputSchema: {
      type: "object",
      properties: {
        match,
        region,
        gain: { type: "number", minimum: 0.05, maximum: 5 },
        deadzone: { type: "number", minimum: 0.001, maximum: 0.5 },
        minPixels: { type: "integer", minimum: 1, maximum: 1000 },
        ms: { type: "integer", minimum: 200, maximum: 10000 },
        wait: { type: "boolean" },
      },
      required: ["match"],
      additionalProperties: false,
    },
  },
  {
    name: "stop",
    description: "Let go of everything: stops the servo, releases all keys and buttons. Call between phases.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
  {
    name: "status",
    description:
      "Whether a session page is attached and has the harness; also locked (the picture has the mouse captured — camera moves only work then) and serving (a servo is running).",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
  {
    name: "guide",
    description:
      "The full playbook for driving this harness: attaching, the fast loop (sense over screenshots, action sequences, wait_for), aiming with servo and tuning it, a worked turn, and troubleshooting. Read this once before your first drive; it needs no session page.",
    inputSchema: { type: "object", properties: {}, additionalProperties: false },
  },
] as const;

/** The command each tool sends to the session page. */
export const TOOL_OPS: Record<string, "view" | "input" | "pad_stream" | "wait_change" | "status" | "sense" | "wait_for" | "servo" | "stop"> = {
  view_frame: "view",
  input: "input",
  pad_stream: "pad_stream",
  wait_for_change: "wait_change",
  sense_frame: "sense",
  wait_for: "wait_for",
  servo: "servo",
  stop: "stop",
  status: "status",
};

/**
 * The playbook an agent drives this harness from: how to attach, the fast loop, aim.
 * Served by the "guide" tool (answered here, no page needed) and summarised in the
 * initialize instructions.
 */
export const GUIDE = [
  "# Driving the Cha dev harness",
  "",
  "You drive a live session through this page's input harness: input goes in as real-shaped",
  "keyboard and pointer events, the picture comes back as data. Dev builds only, our own app.",
  "",
  "## Before you drive",
  "- A session page must be open, signed in with controls, at this environment. status tells you:",
  "  harness (installed), locked (the picture has the mouse captured), serving (a servo runs).",
  "- The mouse is captured by a real person's click on the picture, once. Pointer lock cannot be",
  "  taken synthetically: look and servo move no camera until then, and after any unlock (a held",
  "  Esc, the browser recapturing) someone has to click again. status.locked is the source of truth;",
  "  check it before any camera phase instead of finding out from a stuck picture.",
  "- Starting a run: input with the seed action makes the timing replayable.",
  "",
  "## The fast loop",
  "Every call costs a round trip, and a screenshot costs an image read on top. So:",
  "- Act and observe in one call: input with sense:true adds a colour grid taken settleMs after",
  "  the action. Never look at a screenshot to check whether your own action landed.",
  "- Reason over sense_frame grids, not screenshots. view_frame is for when you must genuinely",
  "  see; take a small region, not the whole picture.",
  "- One call, one mission: input with actions:[...] runs steps in order (their ms must total",
  "  20000 or less). Sprint, jump and turn in a single round trip.",
  "- wait_for closes a check inside the page, on the picture's own frames: \"turn until the door's",
  "  blue fills the right third\" is one call, not ten polls.",
  "- Keep the character alive while you think: a hold with wait:false returns at once and the key",
  "  stays down across calls. stop releases everything and stops any servo; use it between phases.",
  "",
  "## Aim (first-person cameras)",
  "servo holds a colour target centred at frame rate, in the page. You set the policy: match (what",
  "the target looks like: a channel and a range), optionally region, gain, ms. Start at gain 0.5,",
  "deadzone 0.02, every 3. If it oscillates around the target, lower the gain or raise every; if it",
  "lagged behind, raise the gain. It ends itself when the target is lost, ms passes, or the pointer",
  "unlocked, and says which in the result. wait:false starts it and lets you do other things.",
  "",
  "## Controller",
  "The pad actions drive an emulated XInput pad as the session's own pad (slot 0): a real pad",
  "opened on the same page would fight it, so use one or the other. Prefer the shaped actions over",
  "the exact ones — they are what a hand actually does, and what the stream should see:",
  "- pad_stick {axes:{lx:-1}, ms:1200} — a thumb tilt: rise, tremor while held, spring release that",
  "  overshoots centre. style:\"flick\" snaps the rise and counter-pulls harder; use it for camera",
  "  swings on the right stick, and wait:false to keep walking while you think.",
  "- pad_tap {code:\"a\", count:3} — a mash that tires; pad_pull {trigger:\"rt\", depth:0.75} — an",
  "  analog trigger pulled like a finger (full depth is a gun's trigger, half a car's throttle).",
  "- pad {buttons, axes, ms} sets an exact state, no shaping — for tests that need known values.",
  "status.pad says whether anything is held; stop releases the pad with the keys.",
  "",
  "## A worked turn",
  "1. status — is locked true? If not, ask the human to click the picture.",
  "2. input, actions [{action:\"hold\",code:\"KeyW\",ms:800},{action:\"look\",dx:90}], sense:true.",
  "3. sense_frame — read the grid: where did the bright thing end up?",
  "4. wait_for match {channel:\"b\",min:120} in the region you expect it, fraction 0.3.",
  "5. servo match {channel:\"r\",min:150}, ms 4000 — aim held; then input click, then stop.",
  "",
  "## If something doesn't work",
  "- \"the harness isn't installed\" — the page isn't open with controls at this environment.",
  "- The camera will not move — locked is false; only a human's click fixes that.",
  "- The pad seems ignored — a real controller on this page owns the same slot; unplug it.",
  "- \"didn't answer in time\" — the tab is probably backgrounded; bring it forward and retry.",
  "- The timing varies a little on purpose: typing, moving, clicking and stick moves follow",
  "  human-shaped, seeded patterns, so the same seed replays the same run.",
].join("\n");

export type Call = (name: string, args: Record<string, unknown>) => Promise<ToolResult>;

/** Rejects bad input commands before they reach the page. */
function checkInput(args: Record<string, unknown>): string | null {
  if (Array.isArray(args.actions)) {
    let total = 0;
    for (const step of args.actions) {
      if (typeof step !== "object" || step === null) return "actions: every step needs to be an object";
      const a = step as Record<string, unknown>;
      if (!INPUT_ACTIONS.includes(a.action as (typeof INPUT_ACTIONS)[number])) {
        return "every action needs an action: " + INPUT_ACTIONS.join(", ");
      }
      if (typeof a.ms === "number") total += a.ms;
    }
    if (total > 20000) return "actions: the steps' ms add up past 20000; split the maneuver";
    return null;
  }
  if (!INPUT_ACTIONS.includes(args.action as (typeof INPUT_ACTIONS)[number])) {
    return "input needs an action: " + INPUT_ACTIONS.join(", ") + " — or actions: [ ... ]";
  }
  return null;
}

function isMatch(v: unknown): boolean {
  if (typeof v !== "object" || v === null) return false;
  const c = (v as Record<string, unknown>).channel;
  return c === "r" || c === "g" || c === "b";
}

/** The reply for one tool call, given what the session page said. */
export function renderResult(name: string, reply: PageReply): ToolResult {
  if (!reply.ok) return { isError: true, content: [{ type: "text", text: reply.error ?? "the session page failed" }] };
  if (name === "view_frame") {
    const r = reply.result as { w: number; h: number; png: string };
    return {
      content: [
        { type: "image", data: r.png, mimeType: "image/png" },
        { type: "text", text: `${r.w}x${r.h} picture` },
      ],
    };
  }
  return { content: [{ type: "text", text: JSON.stringify(reply.result ?? { ok: true }) }] };
}

interface RpcRequest {
  jsonrpc?: unknown;
  id?: string | number | null;
  method?: unknown;
  params?: Record<string, unknown>;
}

const rpcError = (id: RpcRequest["id"], code: number, message: string) => ({
  jsonrpc: "2.0",
  id: id ?? null,
  error: { code, message },
});

/**
 * One JSON-RPC message in, its reply out. Null for a notification (no id), which gets no body.
 * `call` runs a tool; unknown methods and tools are protocol errors, bad arguments are tool errors.
 */
export async function handleRpc(msg: unknown, call: Call): Promise<object | null> {
  if (typeof msg !== "object" || msg === null || Array.isArray(msg)) {
    return rpcError(null, -32600, "expected one JSON-RPC message");
  }
  const req = msg as RpcRequest;
  const isNotification = !("id" in req);
  const id = req.id ?? null;
  const method = typeof req.method === "string" ? req.method : "";

  if (isNotification) return null;

  switch (method) {
    case "initialize":
      return {
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: PROTOCOL,
          capabilities: { tools: {} },
          serverInfo: { name: "cha-dev-harness", version: "0.1.0" },
          instructions:
            "Drives a live session page's dev harness: input as real-shaped keyboard, pointer and controller events, the picture back as data. The page must be open at this environment, signed in with controls, with the picture clicked once by a person — pointer lock is the one thing a human provides, and look/servo are dead without it (status.locked). Fast loop: input with actions and sense:true, sense_frame instead of view_frame, wait_for for in-page checks, servo to hold an aim, pad_stick/pad_tap/pad_pull for a controller, stop between phases. Call the guide tool for the full playbook with examples.",
        },
      };
    case "ping":
      return { jsonrpc: "2.0", id, result: {} };
    case "tools/list":
      return { jsonrpc: "2.0", id, result: { tools: TOOLS } };
    case "tools/call": {
      const name = typeof req.params?.name === "string" ? req.params.name : "";
      const args = (req.params?.arguments as Record<string, unknown> | undefined) ?? {};
      // The guide is self-documentation: answered here, so it works with no page attached.
      if (name === "guide") return { jsonrpc: "2.0", id, result: { content: [{ type: "text", text: GUIDE }] } };
      if (!(name in TOOL_OPS)) return rpcError(id, -32602, `unknown tool: ${name}`);
      if (name === "input") {
        const bad = checkInput(args);
        if (bad) return { jsonrpc: "2.0", id, result: { isError: true, content: [{ type: "text", text: bad }] } };
      }
      if (name === "pad_stream" && !Array.isArray(args.steps)) {
        return { jsonrpc: "2.0", id, result: { isError: true, content: [{ type: "text", text: "pad_stream needs steps: [{ buttons, axes }, ...]" }] } };
      }
      if ((name === "wait_for" || name === "servo") && !isMatch(args.match)) {
        return { jsonrpc: "2.0", id, result: { isError: true, content: [{ type: "text", text: `${name} needs a match with a channel: r, g or b` }] } };
      }
      return { jsonrpc: "2.0", id, result: await call(name, args) };
    }
    default:
      return rpcError(id, -32601, `method not found: ${method}`);
  }
}
