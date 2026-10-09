// Dev only: connects a session page to the portal's dev MCP endpoint (web/apps/portal/dev-mcp),
// so the harness in `window.__chaHarness` can be driven by an MCP client at
// /environments/<id>/session/mcp. The server answers 404 unless it was started with CHA_DEV_MCP=1,
// and EventSource gives up on a non-200 response, so a page with the bridge off just stops here.
// Commands arrive as SSE `data:` lines; each gets one POST back with its result.
//
// The ops are shaped for a slow driver on a fast page: one command should carry a whole
// maneuver, and cheap summaries (`sense`, digests) should stand in for screenshots, so the
// driver's round trips set the strategy, not the frame rate. Loops that need frame rate
// (`wait_for`, `servo`) close inside the page, on the picture's own frames.
import type { ChannelMatch, Harness, Region, ServoRun, TrackPolicy } from "./harness";
import { matchFraction } from "./harness";

interface Command {
  id: string;
  op: "view" | "input" | "pad_stream" | "wait_change" | "status" | "sense" | "wait_for" | "servo" | "stop";
  args: Record<string, unknown>;
}

type Reply = { ok: true; result: unknown } | { ok: false; error: string };

const num = (v: unknown, fallback: number): number => (typeof v === "number" && Number.isFinite(v) ? v : fallback);
const clamp = (v: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, v));
const str = (v: unknown, what: string): string => {
  if (typeof v !== "string" || !v) throw new Error(`${what} needs a key code, e.g. KeyW`);
  return v;
};
const settle = (ms: number): Promise<void> => new Promise<void>((r) => setTimeout(r, ms));

const WHOLE: Region = { x: 0, y: 0, w: 1, h: 1 };

function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

function region(v: unknown): Region | undefined {
  if (!v || typeof v !== "object") return undefined;
  const r = v as Record<string, unknown>;
  return {
    x: clamp(num(r.x, 0), 0, 1),
    y: clamp(num(r.y, 0), 0, 1),
    w: clamp(num(r.w, 1), 0, 1),
    h: clamp(num(r.h, 1), 0, 1),
  };
}

function matchOf(v: unknown): ChannelMatch {
  if (!v || typeof v !== "object") throw new Error("needs a match: { channel: \"r\" | \"g\" | \"b\", min?, max? }");
  const m = v as Record<string, unknown>;
  if (m.channel !== "r" && m.channel !== "g" && m.channel !== "b") throw new Error("a match needs a channel: r, g or b");
  return { channel: m.channel, min: clamp(num(m.min, 0), 0, 255), max: clamp(num(m.max, 255), 0, 255) };
}

/** The colour grid a driver reasons over, in place of a screenshot. */
function sense(h: Harness, a: Record<string, unknown>) {
  const g = h.grid(clamp(Math.round(num(a.cols, 8)), 2, 32));
  if (!g) throw new Error("no picture yet");
  return g;
}

/** One input action, the way a person's devices would send it. */
async function input(h: Harness, a: Record<string, unknown>): Promise<Record<string, unknown>> {
  const ms = (fallback: number) => clamp(num(a.ms, fallback), 0, 10000);
  // Long holds go on without the command waiting on them: the character keeps
  // moving while the driver thinks. A stray failure here is not actionable.
  const fire = (p: Promise<unknown>): Promise<Record<string, unknown>> => {
    if (a.wait !== false) return p.then(() => ({ ok: true }));
    void p.catch(() => {});
    return Promise.resolve({ ok: true, async: true });
  };
  switch (a.action) {
    case "hold":
      return fire(h.hold(str(a.code, "hold"), ms(500)));
    case "tap":
      await h.tap(str(a.code, "tap"));
      return { ok: true };
    case "chord":
      if (!Array.isArray(a.codes) || a.codes.length === 0) throw new Error("chord needs codes, e.g. [\"KeyW\", \"ShiftLeft\"]");
      return fire(h.chord(a.codes.map((c) => str(c, "chord")), ms(500)));
    case "pad":
      return fire(h.padHold((a.buttons ?? {}) as Record<string, unknown>, (a.axes ?? {}) as Record<string, unknown>, ms(500)));
    case "pad_stick":
      return fire(h.padStick((a.axes ?? {}) as Record<string, unknown>, ms(600), a.style === "flick" ? "flick" : "tilt"));
    case "pad_tap":
      await h.padTap(str(a.code, "pad_tap"), clamp(Math.round(num(a.count, 1)), 1, 32));
      return { ok: true };
    case "pad_pull":
      await h.padPull(a.trigger === "rt" ? "rt" : "lt", ms(400), clamp(num(a.depth, 1), 0, 1));
      return { ok: true };
    case "type":
      if (typeof a.text !== "string") throw new Error("type needs text");
      await h.type(a.text);
      return { ok: true };
    case "look":
      await h.look(num(a.dx, 0), num(a.dy, 0), ms(400));
      return { ok: true };
    case "move_to":
      await h.moveTo(clamp(num(a.fx, 0.5), 0, 1), clamp(num(a.fy, 0.5), 0, 1), ms(600));
      return { ok: true };
    case "click":
      await h.click(clamp(Math.round(num(a.button, 0)), 0, 4));
      return { ok: true };
    case "wait":
      await h.wait(ms(500));
      return { ok: true };
    case "seed":
      h.seed(Math.round(num(a.seed, 1)));
      return { ok: true };
    default:
      throw new Error(`unknown action: ${String(a.action)}`);
  }
}

/** An input command: one action or a sequence, with an optional digest of the picture after. */
async function inputOp(h: Harness, a: Record<string, unknown>): Promise<Record<string, unknown>> {
  let result: Record<string, unknown>;
  if (Array.isArray(a.actions)) {
    const done = [];
    for (const step of a.actions as Record<string, unknown>[]) done.push(await input(h, step));
    result = { done: done.length };
  } else {
    result = await input(h, a);
  }
  if (a.sense === true) {
    await settle(clamp(num(a.settleMs, 80), 0, 1000));
    return { ...result, grid: sense(h, a) };
  }
  return result;
}

/** The servo a `stop` lets go of; one at a time, a new one replaces the old. */
let activeServo: ServoRun | null = null;

async function run(cmd: Command): Promise<Reply> {
  const h = window.__chaHarness;
  if (!h) {
    return { ok: false, error: "the harness isn't installed on this page: open the picture's control channel (you need the controls)" };
  }
  try {
    switch (cmd.op) {
      case "status":
        return { ok: true, result: { harness: true, locked: h.locked, serving: activeServo !== null, pad: h.padHeld } };
      case "view": {
        const png = await h.png(num(cmd.args.width, 256), region(cmd.args.region));
        if (!png) return { ok: false, error: "no picture yet" };
        return { ok: true, result: { w: png.w, h: png.h, png: toBase64(png.data) } };
      }
      case "sense":
        return { ok: true, result: sense(h, cmd.args) };
      case "input":
        return { ok: true, result: await inputOp(h, cmd.args) };
      case "wait_change": {
        const f = await h.waitChange(clamp(num(cmd.args.timeoutMs, 5000), 100, 20000));
        return { ok: true, result: { changed: f !== null } };
      }
      case "wait_for": {
        const m = matchOf(cmd.args.match);
        const where = region(cmd.args.region) ?? WHOLE;
        const fraction = clamp(num(cmd.args.fraction, 0.5), 0.01, 1);
        const f = await h.waitFor(
          (fr) => matchFraction(fr, where, m) >= fraction,
          clamp(num(cmd.args.timeoutMs, 5000), 100, 20000),
          clamp(Math.round(num(cmd.args.every, 2)), 1, 10),
          clamp(Math.round(num(cmd.args.width, 64)), 64, 256),
        );
        const seen = f ?? h.frame(64);
        return { ok: true, result: { matched: f !== null, fraction: seen ? matchFraction(seen, where, m) : 0 } };
      }
      case "servo": {
        const policy: TrackPolicy = {
          match: matchOf(cmd.args.match),
          region: region(cmd.args.region),
          gain: num(cmd.args.gain, 0.7),
          deadzone: num(cmd.args.deadzone, 0.01),
          minPixels: Math.round(num(cmd.args.minPixels, 4)),
        };
        activeServo = h.servo(policy, clamp(num(cmd.args.ms, 4000), 200, 10000));
        if (cmd.args.wait === false) return { ok: true, result: { started: true } };
        return { ok: true, result: await activeServo.result };
      }
      case "pad_stream": {
        // One run of pad states at frame rate; by default it returns at once, so a driver can queue the next.
        const raw = Array.isArray(cmd.args.steps) ? (cmd.args.steps as { buttons?: Record<string, unknown>; axes?: Record<string, unknown> }[]) : [];
        const steps = raw.slice(0, 120).map((s) => ({
          buttons: s && typeof s.buttons === "object" && s.buttons ? s.buttons : {},
          axes: s && typeof s.axes === "object" && s.axes ? s.axes : {},
        }));
        const run = h.padStream(steps, clamp(num(cmd.args.frame_ms, 1000 / 60), 8, 100), clamp(num(cmd.args.hold_ms, 250), 0, 2000));
        if (cmd.args.wait === true) {
          await run;
          return { ok: true, result: { played: steps.length } };
        }
        void run.catch(() => {});
        return { ok: true, result: { queued: steps.length } };
      }
      case "stop": {
        activeServo?.stop();
        activeServo = null;
        h.releaseAll();
        return { ok: true, result: { stopped: true } };
      }
    }
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

/** Connects this page to the dev MCP bridge for its environment. Once per page. */
export function startDevBridge(): void {
  const m = /^\/environments\/([A-Za-z0-9-]+)\/session/.exec(location.pathname);
  if (!m || window.__chaDevBridge) return;
  window.__chaDevBridge = true;

  const base = `/environments/${m[1]}/session/mcp/bridge`;
  const es = new EventSource(base);
  es.onmessage = (e: MessageEvent<string>) => {
    const cmd = JSON.parse(e.data) as Command;
    void run(cmd).then((reply) =>
      fetch(`${base}/result`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id: cmd.id, ...reply }),
      }),
    );
  };
}

declare global {
  interface Window {
    __chaDevBridge?: boolean;
  }
}
