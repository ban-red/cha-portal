// Validates the in-stream UI spec (web/packages/ui-spec, ADR 0016) and that the portal only uses
// icons it holds. The native side's coverage is compile-time: crates/cha-ui-spec generates an `Icon`
// enum from icons.json, so a misspelt icon in Rust does not build.
//
//   bun scripts/check-ui-spec.ts
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const ICONS = join(ROOT, "web/packages/ui-spec/icons.json");
const PORTAL_SRC = join(ROOT, "web/apps/portal/src");
const errors: string[] = [];
const fail = (msg: string) => errors.push(msg);

// --- icons.json ---

const text = readFileSync(ICONS, "utf8");
const icons = JSON.parse(text) as Record<string, unknown>;

// JSON.parse keeps the last duplicate key, so look for repeats in the source text.
const seen = new Set<string>();
for (const m of text.matchAll(/^ {2}"([^"]+)":/gm)) {
  if (seen.has(m[1]!)) fail(`icons.json: id "${m[1]}" appears twice`);
  seen.add(m[1]!);
}

const ARGS: Record<string, number> = { m: 2, l: 2, h: 1, v: 1, c: 6, s: 4, q: 4, t: 2, a: 7, z: 0 };

/** Checks SVG path data the way crates/cha-ui-spec's parser reads it: M first, known commands, whole argument groups. */
function pathProblem(d: string): string | null {
  const s = d.trim();
  if (!/^[Mm]/.test(s)) return "does not start with M";
  const num = String.raw`[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?`;
  const re = new RegExp(String.raw`([A-Za-z])|(${num})|([\s,]+)|(.)`, "gy");
  let cmd = "";
  let count = 0;
  let args: string[] = [];
  const close = (): string | null => {
    const n = ARGS[cmd.toLowerCase()];
    if (n === undefined) return null;
    if (n === 0 ? args.length > 0 : args.length === 0 || args.length % n !== 0)
      return `${cmd} takes groups of ${n} numbers, got ${args.length}`;
    return null;
  };
  let m: RegExpExecArray | null;
  while ((m = re.exec(s))) {
    if (m[1]) {
      const p = close();
      if (p) return p;
      if (!(m[1].toLowerCase() in ARGS)) return `unknown command ${m[1]}`;
      cmd = m[1];
      args = [];
      count++;
    } else if (m[2]) {
      if (!cmd) return "a number before any command";
      // Arc flags are single digits and may run into the next number ("011 1").
      if (cmd.toLowerCase() === "a" && [3, 4].includes(args.length % 7) && /^[01]/.test(m[2]) && m[2].length > 1) {
        args.push(m[2][0]!);
        re.lastIndex -= m[2].length - 1;
        continue;
      }
      args.push(m[2]);
    } else if (m[4]) return `unexpected "${m[4]}"`;
  }
  const p = close();
  if (p) return p;
  return count === 0 ? "empty" : null;
}

for (const [id, icon] of Object.entries(icons)) {
  const at = `icons.json "${id}"`;
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(id)) fail(`${at}: ids are kebab-case`);
  const i = icon as { viewBox?: unknown; parts?: unknown; linecap?: unknown; linejoin?: unknown };
  if (i.viewBox !== 16) fail(`${at}: viewBox must be 16`);
  if (i.linecap !== undefined && !["round", "butt", "square"].includes(i.linecap as string)) fail(`${at}: bad linecap`);
  if (i.linejoin !== undefined && !["round", "bevel", "miter"].includes(i.linejoin as string)) fail(`${at}: bad linejoin`);
  const parts = i.parts;
  if (!Array.isArray(parts) || parts.length === 0) {
    fail(`${at}: needs parts`);
    continue;
  }
  parts.forEach((part: { d?: unknown; stroke?: unknown; fill?: unknown }, n) => {
    const p = `${at} part ${n}`;
    if (typeof part.d !== "string" || !part.d) return fail(`${p}: needs d`);
    const problem = pathProblem(part.d);
    if (problem) fail(`${p}: path "${part.d}" ${problem}`);
    const hasStroke = part.stroke !== undefined;
    const hasFill = part.fill !== undefined;
    if (hasStroke === hasFill) fail(`${p}: needs exactly one of stroke and fill`);
    if (hasStroke && !(typeof part.stroke === "number" && part.stroke > 0)) fail(`${p}: stroke is a width above 0`);
    if (hasFill && part.fill !== true) fail(`${p}: fill is true`);
    if (hasFill && Object.keys(part).length !== 2) fail(`${p}: unknown keys`);
  });
}

// --- every <Icon name="..."> in the portal exists ---

function* files(dir: string): Generator<string> {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) yield* files(p);
    else if (p.endsWith(".vue") || p.endsWith(".ts")) yield p;
  }
}

let used = 0;
for (const file of files(PORTAL_SRC)) {
  const src = readFileSync(file, "utf8");
  if (!/<Icon\b/.test(src)) continue;
  const rel = file.slice(ROOT.length + 1);
  // <Icon name="x" ...> and <Icon :name="cond ? 'a' : 'b'" ...>: every quoted id inside counts.
  for (const tag of src.matchAll(/<Icon\b([^>]*?)\/?>/gs)) {
    const attrs = tag[1]!;
    const lit = attrs.match(/(?:^|\s)name="([^"]+)"/);
    const dyn = attrs.match(/:name="([^"]+)"/);
    const ids = lit ? [lit[1]!] : dyn ? [...dyn[1]!.matchAll(/'([^']+)'/g)].map((m) => m[1]!) : [];
    if (!ids.length) fail(`${rel}: <Icon> without a readable name`);
    for (const id of ids) {
      used++;
      if (!Object.hasOwn(icons, id)) fail(`${rel}: <Icon> uses "${id}", which icons.json does not have`);
    }
  }
}

if (errors.length) {
  for (const e of errors) console.error(e);
  console.error(`ui-spec check failed: ${errors.length} problem(s)`);
  process.exit(1);
}
console.log(`ui-spec ok: ${Object.keys(icons).length} icons, ${used} uses in the portal`);
