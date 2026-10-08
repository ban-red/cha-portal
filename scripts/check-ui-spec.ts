// Validates the in-stream UI spec (web/packages/ui-spec, ADR 0016) and that the portal only uses
// icons it holds. The native side's coverage is compile-time: crates/cha-ui-spec generates an `Icon`
// enum from icons.json, so a misspelt icon in Rust does not build. For health.json it checks the
// issues (ids, platforms, wording, template placeholders) and health-cases.json (known issue ids,
// severities and fields); the checks' own coverage is in each player's tests.
//
//   bun scripts/check-ui-spec.ts
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

const ROOT = join(import.meta.dir, "..");
const ICONS = join(ROOT, "web/packages/ui-spec/icons.json");
const HEALTH = join(ROOT, "web/packages/ui-spec/health.json");
const HEALTH_CASES = join(ROOT, "web/packages/ui-spec/health-cases.json");
const FILL_CASES = join(ROOT, "web/packages/ui-spec/fill-cases.json");
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

// --- health.json and its cases ---

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);
const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const PLATFORMS = ["web", "native"];
const FORMATTERS = ["f0", "f1", "f2", "ms0", "ms1"];
const SEVERITIES = ["minor", "major", "critical"];
const GRADES = ["A", "B", "C", "D", "F"];
/** The same pattern as fill() in index.ts and cha-ui-spec's health.rs. */
const PLACEHOLDER = /\{([a-z][a-z0-9_]*)(?::([a-z0-9]+))?\}/g;
const SNAPSHOT_FIELDS = [
  "codec", "target_fps", "sent_fps", "shown_sent_fps", "shown_fps", "encode_p99_ms", "decode_ms", "jitter_buffer_ms",
  "rtt_ms", "lost", "recovered", "partial", "dropped", "latency_ms", "delivery_ms", "delivery_p95_ms", "frame_gap_ms",
  "audio_jitter_ms", "audio_restarts", "audio_in_peak", "audio_out_peak", "node", "awdl_suspected",
];
const NODE_FIELDS = ["cpu", "cores", "mem_used", "mem_total", "gpu", "vram_used", "vram_total", "enc", "streamer_cpu"];

const health = JSON.parse(readFileSync(HEALTH, "utf8")) as Obj;
const issueSpecs = (Array.isArray(health.issues) ? health.issues : []) as Obj[];
const issuePlatforms = new Map<string, string[]>();

/** The platforms a text covers: all of the issue's for a string, the keys of an object. */
function textPlatforms(t: unknown, allowed: string[], at: string): string[] {
  if (typeof t === "string") return allowed;
  if (!isObj(t)) {
    fail(`${at}: a string or { web?, native? }`);
    return [];
  }
  for (const k of Object.keys(t)) {
    if (!allowed.includes(k)) fail(`${at}: "${k}" is not one of the issue's platforms (${allowed.join(", ")})`);
    else if (typeof t[k] !== "string") fail(`${at}.${k}: must be a string`);
  }
  return Object.keys(t).filter((k) => allowed.includes(k));
}

for (const key of ["window", "min_snapshots", "fallback_fps"]) {
  if (!isNum(health[key]) || (health[key] as number) <= 0) fail(`health.json: ${key} is a positive number`);
}
const levels = (isObj(health.levels) ? health.levels : {}) as Record<string, number>;
if (!(levels.min! > 0 && levels.min! < levels.major! && levels.major! < levels.critical! && levels.critical! <= 1)) {
  fail("health.json: levels need 0 < min < major < critical <= 1");
}
for (const k of ["base", "slope", "spike_share"]) if (!isNum(levels[k])) fail(`health.json: levels.${k} is a number`);
const cap = (isObj(health.severity_cap) ? health.severity_cap : {}) as Record<string, number>;
if (!(cap.minor! > cap.major! && cap.major! > cap.critical!)) fail("health.json: severity_cap falls from minor to critical");
const grades = (Array.isArray(health.grades) ? health.grades : []) as { grade?: string; floor?: number }[];
if (grades.map((g) => g.grade).join("") !== GRADES.join("")) fail(`health.json: grades are ${GRADES.join(", ")} in that order`);
grades.forEach((g, i) => {
  if (!isNum(g.floor) || (i > 0 && g.floor >= grades[i - 1]!.floor!)) fail("health.json: grade floors fall");
});
if (grades.at(-1)?.floor !== 0) fail("health.json: the last grade's floor is 0");
for (const [group, o] of [["params", health.params], ["bands", health.bands]] as const) {
  if (!isObj(o)) {
    fail(`health.json: ${group} is an object`);
    continue;
  }
  for (const [k, v] of Object.entries(o)) {
    if (group === "params") {
      if (!isNum(v)) fail(`health.json: params.${k} is a number`);
    } else if (!isObj(v) || !isNum(v.from) || !isNum(v.to) || v.from === v.to || Object.keys(v).length !== 2) {
      fail(`health.json: bands.${k} is { from, to } with two different numbers`);
    }
  }
}
if (!isObj(health.text) || !["hidden", "measuring", "smooth", "list_separator"].every((k) => typeof (health.text as Obj)[k] === "string")) {
  fail("health.json: text needs hidden, measuring, smooth and list_separator strings");
}

const seenIssues = new Set<string>();
let placeholderUses = 0;
for (const issue of issueSpecs) {
  const id = String(issue.id);
  const at = `health.json issue "${id}"`;
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(id)) fail(`${at}: ids are kebab-case`);
  if (seenIssues.has(id)) fail(`${at}: id appears twice`);
  seenIssues.add(id);
  const platforms = Array.isArray(issue.platforms) ? (issue.platforms as string[]) : [];
  if (!platforms.length || new Set(platforms).size !== platforms.length || platforms.some((p) => !PLATFORMS.includes(p))) {
    fail(`${at}: platforms is a non-empty list of ${PLATFORMS.join(", ")}`);
    continue;
  }
  issuePlatforms.set(id, platforms);
  if (typeof issue.summary !== "string" || !issue.summary) fail(`${at}: needs a summary`);
  if (!isNum(issue.weight) || issue.weight <= 0) fail(`${at}: weight is a positive number`);
  for (const field of ["title", "hint"] as const) {
    const covered = textPlatforms(issue[field], platforms, `${at} ${field}`);
    for (const p of platforms) if (!covered.includes(p)) fail(`${at}: ${field} has no wording for ${p}`);
  }
  const details = isObj(issue.details) ? issue.details : {};
  if (!Object.keys(details).length) fail(`${at}: needs a detail`);
  const coveredDetail = new Set<string>();
  const texts: [string, string][] = [];
  const collect = (label: string, t: unknown) => {
    for (const p of textPlatforms(t, platforms, `${at} ${label}`)) coveredDetail.add(p);
    if (typeof t === "string") texts.push([label, t]);
    else if (isObj(t)) for (const [k, v] of Object.entries(t)) if (typeof v === "string") texts.push([`${label}.${k}`, v]);
  };
  for (const [variant, t] of Object.entries(details)) collect(`detail "${variant}"`, t);
  for (const p of platforms) if (!coveredDetail.has(p)) fail(`${at}: no detail for ${p}`);
  collect("title", issue.title);
  collect("hint", issue.hint);
  // Placeholders: declared, spelt right, with a formatter exactly when they are numbers.
  const declared = isObj(issue.placeholders) ? (issue.placeholders as Record<string, unknown>) : {};
  for (const [name, kind] of Object.entries(declared)) {
    if (kind !== "number" && kind !== "string") fail(`${at}: placeholder "${name}" is "number" or "string"`);
  }
  const used = new Set<string>();
  for (const [label, text] of texts) {
    for (const m of text.matchAll(PLACEHOLDER)) {
      const [, name, fmt] = m as unknown as [string, string, string | undefined];
      used.add(name);
      placeholderUses++;
      const kind = declared[name];
      if (kind === undefined) fail(`${at} ${label}: placeholder {${name}} is not declared in "placeholders"`);
      else if (kind === "number" && !fmt) fail(`${at} ${label}: {${name}} is a number and needs a formatter, e.g. {${name}:f0}`);
      else if (kind === "string" && fmt) fail(`${at} ${label}: {${name}:${fmt}} is a string and takes no formatter`);
      if (fmt && !FORMATTERS.includes(fmt)) fail(`${at} ${label}: unknown formatter "${fmt}" (${FORMATTERS.join(", ")})`);
    }
    // A brace that is not part of a valid placeholder is a typo.
    const stray = text.replace(PLACEHOLDER, "");
    if (/[{}]/.test(stray)) fail(`${at} ${label}: a stray brace in ${JSON.stringify(text)}`);
  }
  for (const name of Object.keys(declared)) if (!used.has(name)) fail(`${at}: placeholder "${name}" is declared but no template uses it`);
}

// --- health-cases.json ---

const casesFile = JSON.parse(readFileSync(HEALTH_CASES, "utf8")) as Obj;
const cases = (Array.isArray(casesFile.cases) ? casesFile.cases : []) as Obj[];
if (!isObj(casesFile.base)) fail("health-cases.json: base is an object");
const caseNames = new Set<string>();
const caseCount = { shared: 0, web: 0, native: 0 };

function checkHistory(history: unknown, at: string) {
  if (!Array.isArray(history)) return fail(`${at}: history is a list`);
  for (const [n, entry] of history.entries()) {
    const e = `${at} history[${n}]`;
    if (!isObj(entry)) {
      fail(`${e}: an object`);
      continue;
    }
    const repeat = entry.repeat ?? 1;
    if (!Number.isInteger(repeat) || (repeat as number) < 1) fail(`${e}: repeat is a positive whole number`);
    for (const [k, v] of Object.entries(entry)) {
      if (k === "repeat") continue;
      if (!SNAPSHOT_FIELDS.includes(k)) fail(`${e}: unknown snapshot field "${k}"`);
      if (Array.isArray(v) && v.length !== repeat) fail(`${e}: ${k} has ${v.length} values for repeat ${repeat}`);
      if (k === "node" && isObj(v)) {
        for (const nk of Object.keys(v)) if (!NODE_FIELDS.includes(nk)) fail(`${e}: unknown node field "${nk}"`);
      }
    }
  }
}

for (const c of cases) {
  const name = String(c.name);
  const at = `health-cases.json "${name}"`;
  if (typeof c.name !== "string" || !c.name) fail("health-cases.json: a case without a name");
  if (caseNames.has(name)) fail(`${at}: name appears twice`);
  caseNames.add(name);
  const platforms = c.platforms === undefined ? PLATFORMS : Array.isArray(c.platforms) ? (c.platforms as string[]) : [];
  if (!platforms.length || platforms.some((p) => !PLATFORMS.includes(p))) fail(`${at}: platforms is a non-empty list of ${PLATFORMS.join(", ")}`);
  if (c.platforms === undefined) caseCount.shared++;
  else for (const p of platforms) if (p === "web" || p === "native") caseCount[p]++;
  checkHistory(c.history, at);
  if (c.context !== undefined) {
    const ctx = c.context;
    if (!isObj(ctx) || Object.keys(ctx).some((k) => !["visible", "interval_ms"].includes(k))) fail(`${at}: context has visible and interval_ms`);
    else if ((ctx.visible !== undefined || ctx.interval_ms !== undefined) && platforms.includes("native")) {
      fail(`${at}: only the browser has a hidden page or another interval; give the case "platforms": ["web"]`);
    }
  }
  const e = c.expect;
  if (!isObj(e)) {
    fail(`${at}: needs expect`);
    continue;
  }
  if (e.grade !== null && !GRADES.includes(e.grade as string)) fail(`${at}: grade is one of ${GRADES.join(", ")} or null`);
  if (e.score !== null && !(Number.isInteger(e.score) && (e.score as number) >= 0 && (e.score as number) <= 100)) fail(`${at}: score is 0..100 or null`);
  if ((e.grade === null) !== (e.score === null)) fail(`${at}: grade and score are both null or neither`);
  if (typeof e.summary !== "string" || !e.summary) fail(`${at}: needs a summary`);
  const expectedIds = new Set<string>();
  for (const i of (Array.isArray(e.issues) ? e.issues : []) as Obj[]) {
    const id = String(i.id);
    if (!SEVERITIES.includes(i.severity as string)) fail(`${at}: issue "${id}" has severity "${i.severity}"`);
    if (!issuePlatforms.has(id)) fail(`${at}: issue "${id}" is not in health.json`);
    for (const p of platforms) if (issuePlatforms.has(id) && !issuePlatforms.get(id)!.includes(p)) fail(`${at}: issue "${id}" is not reported on ${p}; give the case the platforms that report it`);
    expectedIds.add(id);
  }
  if (!Array.isArray(e.issues)) fail(`${at}: issues is a list`);
  for (const field of ["details", "hints"] as const) {
    const texts = e[field];
    if (texts === undefined) continue;
    if (!isObj(texts)) {
      fail(`${at}: ${field} is an object`);
      continue;
    }
    for (const [id, t] of Object.entries(texts)) {
      if (!expectedIds.has(id)) fail(`${at}: ${field} for "${id}", which the case does not expect`);
      const covered = textPlatforms(t, PLATFORMS, `${at} ${field}.${id}`);
      for (const p of platforms) if (!covered.includes(p)) fail(`${at}: ${field}.${id} has no text for ${p}`);
    }
  }
}

const fillCases = JSON.parse(readFileSync(FILL_CASES, "utf8")) as Obj[];
const fillNames = new Set<string>();
for (const c of fillCases) {
  const at = `fill-cases.json "${c.name}"`;
  if (typeof c.name !== "string" || fillNames.has(c.name)) fail(`${at}: names are unique strings`);
  fillNames.add(String(c.name));
  if (typeof c.template !== "string" || typeof c.expect !== "string" || !isObj(c.values)) fail(`${at}: needs template, values and expect`);
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
console.log(
  `ui-spec ok: ${Object.keys(icons).length} icons, ${used} uses in the portal; ${issueSpecs.length} health issues ` +
    `(${placeholderUses} placeholders), ${cases.length} health cases (${caseCount.shared} shared, ${caseCount.web} web-only, ${caseCount.native} native-only), ${fillCases.length} fill cases`,
);
