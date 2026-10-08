// Validates the in-stream UI spec (web/packages/ui-spec, ADR 0016) and that the portal only uses
// icons it holds. The native side's coverage is compile-time: crates/cha-ui-spec generates an `Icon`
// enum from icons.json, so a misspelt icon in Rust does not build. For health.json it checks the
// issues (ids, platforms, wording, template placeholders) and health-cases.json (known issue ids,
// severities and fields); the checks' own coverage is in each player's tests. For toolbar.json it checks the
// controls, menus and rows (unique ids, icons that exist, declared capabilities and state keys, platforms,
// conditions, templates) and toolbar-cases.json (every control, row and capability has a case).
//
//   bun scripts/check-ui-spec.ts
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { ROLES } from "../web/apps/portal/src/themes/index.ts";

const ROOT = join(import.meta.dir, "..");
const ICONS = join(ROOT, "web/packages/ui-spec/icons.json");
const HEALTH = join(ROOT, "web/packages/ui-spec/health.json");
const HEALTH_CASES = join(ROOT, "web/packages/ui-spec/health-cases.json");
const FILL_CASES = join(ROOT, "web/packages/ui-spec/fill-cases.json");
const PANEL = join(ROOT, "web/packages/ui-spec/stats-panel.json");
const PANEL_CASES = join(ROOT, "web/packages/ui-spec/stats-panel-cases.json");
const FORMAT_CASES = join(ROOT, "web/packages/ui-spec/format-cases.json");
const STATS_OVERLAY = join(ROOT, "web/apps/portal/src/statsOverlay.ts");
const PREFS = join(ROOT, "web/packages/ui-spec/prefs.json");
const PREFS_CASES = join(ROOT, "web/packages/ui-spec/prefs-cases.json");
const TOOLBAR = join(ROOT, "web/packages/ui-spec/toolbar.json");
const TOOLBAR_CASES = join(ROOT, "web/packages/ui-spec/toolbar-cases.json");
const CAPTURE_CASES = join(ROOT, "web/packages/ui-spec/capture-cases.json");
const THEMES_INDEX = join(ROOT, "web/apps/portal/src/themes/index.ts");
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
const FORMATTERS = ["f0", "f1", "f2", "ms0", "ms1", "int", "gb", "pct", "mbit", "s"];
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

// --- stats-panel.json and its cases ---

const panel = JSON.parse(readFileSync(PANEL, "utf8")) as Obj;
const TONES = ["none", "ok", "warn", "danger", "dim"];
const panelValues = (isObj(panel.values) ? panel.values : {}) as Record<string, Obj>;
// The full view's section ids are the saved settings' `folded` entries (prefs.json): the one list the
// panel's sections, the folded pref, statsOverlay.ts and overlay_prefs.rs's `Section` all follow.
const prefsSpec = JSON.parse(readFileSync(PREFS, "utf8")) as Obj;
const sectionIds = (((prefsSpec.stats_panel as Obj | undefined)?.fields as Obj | undefined)?.folded as Obj | undefined)?.values as string[] ?? [];
const issueIds = new Set(issueSpecs.map((i) => String(i.id)));
let panelRows = 0;
let panelTemplates = 0;

for (const [k, v] of Object.entries(panelValues)) {
  const at = `stats-panel.json value "${k}"`;
  if (!/^[a-z][a-z0-9_]*$/.test(k)) fail(`${at}: snake_case`);
  if (v.kind !== "number" && v.kind !== "string") fail(`${at}: kind is "number" or "string"`);
  const ps = Array.isArray(v.platforms) ? (v.platforms as string[]) : [];
  if (!ps.length || ps.some((p) => !PLATFORMS.includes(p))) fail(`${at}: platforms is a non-empty list of ${PLATFORMS.join(", ")}`);
}

const platformsOf = (c: Obj): string[] => (Array.isArray(c.platforms) ? (c.platforms as string[]) : PLATFORMS);

/** A text's templates, with the platforms each covers. */
function templates(t: unknown, platforms: string[], at: string): [string, string[]][] {
  if (typeof t === "string") return [[t, platforms]];
  if (!isObj(t)) {
    fail(`${at}: a string or { web?, native? }`);
    return [];
  }
  const out: [string, string[]][] = [];
  for (const [p, text] of Object.entries(t)) {
    if (!PLATFORMS.includes(p) || typeof text !== "string") fail(`${at}: "${p}" must be web or native with a string`);
    else out.push([text, [p]]);
  }
  for (const p of platforms) if (!(p in t)) fail(`${at}: no text for ${p}`);
  return out;
}

/** Every key a template uses must be declared, of the right kind, and filled on every platform the template is shown on. */
function checkTemplate(text: string, platforms: string[], at: string, extra: string[] = []) {
  panelTemplates++;
  for (const m of text.matchAll(PLACEHOLDER)) {
    const [, name, fmt] = m as unknown as [string, string, string | undefined];
    const v = panelValues[name!];
    if (!v && !extra.includes(name)) {
      fail(`${at}: {${name}} is not a declared value`);
      continue;
    }
    const kind = v ? v.kind : "string";
    if (fmt && !FORMATTERS.includes(fmt)) fail(`${at}: unknown formatter "${fmt}"`);
    if (kind === "number" && !fmt) fail(`${at}: {${name}} is a number and needs a formatter`);
    if (kind === "string" && fmt) fail(`${at}: {${name}:${fmt}} is a string and takes no formatter`);
    if (v) for (const p of platforms) if (!(v.platforms as string[]).includes(p)) fail(`${at}: {${name}} is not filled on ${p}`);
  }
  if (/[{}]/.test(text.replace(PLACEHOLDER, ""))) fail(`${at}: a stray brace in ${JSON.stringify(text)}`);
}

function checkKeys(keys: unknown, platforms: string[], at: string) {
  const lists: [string[], string[]][] = [];
  if (Array.isArray(keys)) lists.push([keys as string[], platforms]);
  else if (isObj(keys)) {
    for (const [p, l] of Object.entries(keys)) {
      if (!PLATFORMS.includes(p) || !Array.isArray(l)) fail(`${at}: { web?, native? } of key lists`);
      else lists.push([l as string[], [p]]);
    }
  } else if (keys !== undefined) fail(`${at}: a list of value keys`);
  for (const [l, ps] of lists) {
    for (const k of l) {
      const v = panelValues[k];
      if (!v) fail(`${at}: "${k}" is not a declared value`);
      else for (const p of ps) if (!(v.platforms as string[]).includes(p)) fail(`${at}: "${k}" is not filled on ${p}`);
    }
  }
}

function checkHot(hot: unknown, platforms: string[], at: string) {
  if (!isObj(hot) || typeof hot.key !== "string" || !isNum(hot.limit) || Object.keys(hot).some((k) => !["key", "of", "limit"].includes(k))) {
    return fail(`${at}: hot is { key, of?, limit }`);
  }
  checkKeys([hot.key, ...(hot.of === undefined ? [] : [hot.of as string])], platforms, `${at} hot`);
  for (const k of [hot.key, hot.of]) if (typeof k === "string" && panelValues[k]?.kind !== "number") fail(`${at}: hot "${k}" is not a number`);
}

function checkBad(bad: unknown, platforms: string[], at: string) {
  const lists: [string[], string[]][] = [];
  if (Array.isArray(bad)) lists.push([bad as string[], platforms]);
  else if (isObj(bad)) for (const [p, l] of Object.entries(bad)) lists.push([l as string[], PLATFORMS.includes(p) ? [p] : []]);
  else if (bad !== undefined) fail(`${at}: bad is a list of issue ids`);
  for (const [l, ps] of lists) {
    if (!Array.isArray(l)) continue;
    for (const id of l) {
      if (!issueIds.has(id)) fail(`${at}: bad "${id}" is not an issue in health.json`);
      else for (const p of ps) if (!issuePlatforms.get(id)!.includes(p)) fail(`${at}: bad "${id}" is not reported on ${p}`);
    }
  }
}

function checkParts(list: unknown, platforms: string[], at: string, allowRow = false) {
  if (!Array.isArray(list) || !list.length) return fail(`${at}: a non-empty list of parts`);
  (list as Obj[]).forEach((p, n) => {
    const a = `${at}[${n}]`;
    const allowed = ["text", "when", "unless", "platforms", ...(allowRow ? ["hot", "tone"] : [])];
    if (!isObj(p) || Object.keys(p).some((k) => !allowed.includes(k))) return fail(`${a}: a part has ${allowed.join(", ")}`);
    const ps = p.platforms === undefined ? platforms : (p.platforms as string[]).filter((x) => platforms.includes(x));
    if (p.platforms !== undefined && (!Array.isArray(p.platforms) || p.platforms.some((x) => !PLATFORMS.includes(x as string)))) fail(`${a}: platforms`);
    for (const [t, tp] of templates(p.text, ps, `${a} text`)) checkTemplate(t, tp, `${a} text`);
    checkKeys(p.when, ps, `${a} when`);
    checkKeys(p.unless, ps, `${a} unless`);
    if (p.hot !== undefined) checkHot(p.hot, ps, a);
    if (p.tone !== undefined && p.tone !== "dim") fail(`${a}: tone is "dim"`);
  });
}

const seenSections = new Set<string>();
const seenRows = new Set<string>();
if (!Array.isArray(panel.sections)) fail("stats-panel.json: sections is a list");
for (const s of (Array.isArray(panel.sections) ? panel.sections : []) as Obj[]) {
  const sid = String(s.id);
  const at = `stats-panel.json section "${sid}"`;
  if (seenSections.has(sid)) fail(`${at}: id appears twice`);
  seenSections.add(sid);
  if (!sectionIds.includes(sid)) fail(`${at}: not one of prefs.json's folded values (${sectionIds.join(", ")})`);
  if (typeof s.heading !== "string" || !s.heading) fail(`${at}: needs a heading`);
  if (!(ROLES as readonly string[]).includes(s.color as string)) fail(`${at}: color "${s.color}" is not a theme role (${THEMES_INDEX.slice(ROOT.length + 1)})`);
  checkKeys(s.when, PLATFORMS, `${at} when`);
  const sum = s.summary;
  if (!isObj(sum)) fail(`${at}: needs a summary`);
  else {
    checkParts(sum.parts, PLATFORMS, `${at} summary.parts`);
    checkBad(sum.bad, PLATFORMS, `${at} summary`);
    if (sum.hot !== undefined) {
      if (!Array.isArray(sum.hot)) fail(`${at} summary.hot: a list`);
      else for (const h of sum.hot) checkHot(h, PLATFORMS, `${at} summary`);
    }
  }
  for (const r of (Array.isArray(s.rows) ? s.rows : []) as Obj[]) {
    const rid = String(r.id);
    const ra = `${at} row "${rid}"`;
    panelRows++;
    if (seenRows.has(rid)) fail(`${ra}: id appears twice`);
    seenRows.add(rid);
    if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(rid)) fail(`${ra}: ids are kebab-case`);
    const ps = platformsOf(r);
    if (r.platforms !== undefined && (!Array.isArray(r.platforms) || !ps.length || ps.some((p) => !PLATFORMS.includes(p)))) fail(`${ra}: platforms`);
    for (const f of ["label", "tooltip"]) for (const [t, tp] of templates(r[f], ps, `${ra} ${f}`)) if (/[{}]/.test(t)) fail(`${ra} ${f}: no placeholders in ${f}s`) ;
    if (typeof r.value === "string") checkParts([{ text: r.value }], ps, `${ra} value`, true);
    else checkParts(r.value, ps, `${ra} value`, true);
    checkKeys(r.when, ps, `${ra} when`);
    checkBad(r.bad, ps, ra);
  }
}
if (sectionIds.join() !== [...seenSections].join()) fail(`stats-panel.json: sections are ${[...seenSections].join(", ")}, prefs.json's folded values are ${sectionIds.join(", ")}`);

checkParts(panel.compact, PLATFORMS, "stats-panel.json compact");
const report = (isObj(panel.report) ? panel.report : {}) as Obj;
for (const [t, tp] of templates(report.agent, PLATFORMS, "stats-panel.json report.agent")) checkTemplate(t, tp, "stats-panel.json report.agent");
if (typeof report.issue !== "string") fail("stats-panel.json report.issue: a template");
else checkTemplate(report.issue, PLATFORMS, "stats-panel.json report.issue", ["title", "detail"]);
const seenLines = new Set<string>();
for (const l of (Array.isArray(report.lines) ? report.lines : []) as Obj[]) {
  const at = `stats-panel.json report line "${l.id}"`;
  if (seenLines.has(String(l.id))) fail(`${at}: id appears twice`);
  seenLines.add(String(l.id));
  const ps = platformsOf(l);
  checkKeys(l.when, ps, `${at} when`);
  checkParts(l.parts, ps, `${at} parts`);
}

const panelCases = JSON.parse(readFileSync(PANEL_CASES, "utf8")) as Obj[];
const panelNames = new Set<string>();
for (const c of panelCases) {
  const at = `stats-panel-cases.json "${c.name}"`;
  if (typeof c.name !== "string" || panelNames.has(c.name)) fail(`${at}: names are unique strings`);
  panelNames.add(String(c.name));
  const cps = platformsOf(c);
  if (c.platforms !== undefined && (!Array.isArray(c.platforms) || !cps.length || cps.some((p) => !PLATFORMS.includes(p)))) fail(`${at}: platforms`);
  for (const k of Object.keys(isObj(c.values) ? c.values : {})) if (!panelValues[k] || panelValues[k]!.derived) fail(`${at}: "${k}" is not a value a player fills`);
  const h = c.health;
  if (h !== undefined) {
    if (!isObj(h) || (h.grade !== null && !GRADES.includes(h.grade as string)) || (h.grade === null) !== (h.score === null)) fail(`${at}: health needs a grade and score, or neither`);
    else for (const i of (h.issues as Obj[])) if (!issueIds.has(String(i.id)) || !SEVERITIES.includes(i.severity as string)) fail(`${at}: health issue "${i.id}"`);
  }
  const e = c.expect as Obj;
  const expects = isObj(e) && "grade_tone" in e ? [e] : isObj(e) ? Object.values(e) as Obj[] : [];
  if (!isObj(e) || (!("grade_tone" in e) && !cps.every((p) => p in e))) fail(`${at}: expect covers ${cps.join(", ")}`);
  for (const x of expects) {
    if (!TONES.includes(x.grade_tone as string)) fail(`${at}: grade_tone`);
    for (const s of (x.sections as Obj[])) {
      if (!seenSections.has(String(s.id))) fail(`${at}: section "${s.id}" is not in the spec`);
      const known = new Set(((panel.sections as Obj[]).find((q) => q.id === s.id)?.rows as Obj[] | undefined)?.map((r) => String(r.id)));
      for (const id of s.row_ids as string[]) if (!known.has(id)) fail(`${at}: row "${id}" is not in section "${s.id}"`);
      for (const r of (s.rows ?? []) as Obj[]) if (!(s.row_ids as string[]).includes(String(r.id))) fail(`${at}: pinned row "${r.id}" is not among the section's row_ids`);
    }
  }
}

const formatFile = JSON.parse(readFileSync(FORMAT_CASES, "utf8")) as { fill: Obj[]; codec_tag: Obj[] };
const formatNames = new Set<string>();
for (const c of [...formatFile.fill, ...formatFile.codec_tag]) {
  const at = `format-cases.json "${c.name}"`;
  if (typeof c.name !== "string" || formatNames.has(c.name)) fail(`${at}: names are unique strings`);
  formatNames.add(String(c.name));
  if (typeof c.expect !== "string") fail(`${at}: needs expect`);
}
for (const c of formatFile.fill) {
  for (const m of String(c.template).matchAll(PLACEHOLDER)) if (m[2] && !FORMATTERS.includes(m[2])) fail(`format-cases.json "${c.name}": unknown formatter "${m[2]}"`);
}
for (const f of FORMATTERS) {
  if (!formatFile.fill.some((c) => new RegExp(`:${f}\\}`).test(String(c.template)))) fail(`format-cases.json: no case for the formatter "${f}"`);
}

// --- prefs.json and its cases ---

const PREF_TYPES = ["bool", "int", "number", "string", "enum", "list", "object"];
const prefsFile = prefsSpec;
const PREF_GROUPS = ["stats_panel", "toolbar"];
const prefFieldKeys = ["type", "doc", "default", "optional", "platforms", "min", "max", "round", "clamp", "nonempty", "values", "fields"];

/** Why `v` is not a value `f` can hold (the saved form: what the parsers return), or null. */
function prefProblem(f: Obj, v: unknown): string | null {
  switch (f.type) {
    case "bool":
      return typeof v === "boolean" ? null : "not a boolean";
    case "string":
      return typeof v === "string" && (!f.nonempty || v) ? null : "not a (non-empty) string";
    case "enum":
      return typeof v === "string" && (f.values as string[]).includes(v) ? null : `not one of ${(f.values as string[]).join(", ")}`;
    case "list": {
      if (!Array.isArray(v)) return "not a list";
      const order = (f.values as string[]);
      if (v.some((x) => !order.includes(x as string))) return "has an entry outside the values";
      const idx = v.map((x) => order.indexOf(x as string));
      return idx.every((n, i) => i === 0 || n > idx[i - 1]!) ? null : "entries repeat or are out of the values' order";
    }
    case "object": {
      if (!isObj(v)) return "not an object";
      const subs = f.fields as Record<string, Obj>;
      if (Object.keys(v).sort().join() !== Object.keys(subs).sort().join()) return `needs exactly ${Object.keys(subs).join(", ")}`;
      for (const [k, sub] of Object.entries(subs)) {
        const p = prefProblem(sub, v[k]);
        if (p) return `${k}: ${p}`;
      }
      return null;
    }
    case "int":
    case "number": {
      if (!isNum(v)) return "not a finite number";
      if (f.type === "int" && !Number.isInteger(v)) return "not a whole number";
      if (isNum(f.min) && v < f.min) return `below the minimum ${f.min}`;
      if (isNum(f.max) && v > f.max) return `above the maximum ${f.max}`;
      return null;
    }
    default:
      return "unknown type";
  }
}

const prefFieldsOf = (g: string) => (isObj(prefsFile[g]) && isObj((prefsFile[g] as Obj).fields) ? ((prefsFile[g] as Obj).fields as Record<string, Obj>) : {});
const prefPlatforms = (f: Obj) => (Array.isArray(f.platforms) ? (f.platforms as string[]) : PLATFORMS);

function checkPrefField(f: Obj, at: string, top: boolean) {
  const bad = Object.keys(f).filter((k) => !prefFieldKeys.includes(k));
  if (bad.length) fail(`${at}: unknown keys ${bad.join(", ")}`);
  if (!PREF_TYPES.includes(f.type as string)) return fail(`${at}: type is one of ${PREF_TYPES.join(", ")}`);
  if (typeof f.doc !== "string" || !f.doc) fail(`${at}: needs a one-line doc`);
  const hasDefault = "default" in f;
  if (top && hasDefault === (f.optional === true)) fail(`${at}: has either a default or "optional": true`);
  if (!top && (hasDefault || f.optional !== undefined || f.platforms !== undefined)) fail(`${at}: a field of an object has no default, optional or platforms`);
  if (f.optional !== undefined && f.optional !== true) fail(`${at}: optional is true`);
  if (f.platforms !== undefined && (!Array.isArray(f.platforms) || !f.platforms.length || f.platforms.some((p) => !PLATFORMS.includes(p as string)))) fail(`${at}: platforms is a non-empty list of ${PLATFORMS.join(", ")}`);
  const numeric = f.type === "int" || f.type === "number";
  for (const k of ["min", "max"]) if (f[k] !== undefined && (!numeric || !isNum(f[k]))) fail(`${at}: ${k} is a number on an int or number`);
  if (isNum(f.min) && isNum(f.max) && f.min > f.max) fail(`${at}: min is above max`);
  if (f.round !== undefined && (f.round !== true || f.type !== "int")) fail(`${at}: round is true, on an int`);
  if (f.clamp !== undefined && (f.clamp !== true || !numeric || (f.min === undefined && f.max === undefined))) fail(`${at}: clamp is true, on a number with a limit`);
  if (f.nonempty !== undefined && (f.nonempty !== true || f.type !== "string")) fail(`${at}: nonempty is true, on a string`);
  if (f.type === "enum" || f.type === "list") {
    const v = f.values;
    if (!Array.isArray(v) || !v.length || v.some((x) => typeof x !== "string") || new Set(v).size !== v.length) fail(`${at}: values is a list of distinct strings`);
  } else if (f.values !== undefined) fail(`${at}: values is for an enum or list`);
  if (f.type === "object") {
    if (!isObj(f.fields) || !Object.keys(f.fields).length) fail(`${at}: an object needs fields`);
    else for (const [k, sub] of Object.entries(f.fields)) if (isObj(sub)) checkPrefField(sub, `${at}.${k}`, false);
  } else if (f.fields !== undefined) fail(`${at}: fields is for an object`);
  if (hasDefault && f.default !== null) {
    const p = prefProblem(f, f.default);
    if (p) fail(`${at}: default ${JSON.stringify(f.default)} is ${p}`);
  }
  if (hasDefault && f.default === null && f.type !== "object") fail(`${at}: only an object may default to null`);
}

if (Object.keys(prefsFile).filter((k) => k !== "notes").join() !== PREF_GROUPS.join()) fail(`prefs.json: groups are ${PREF_GROUPS.join(", ")}`);
for (const g of PREF_GROUPS) {
  for (const [name, f] of Object.entries(prefFieldsOf(g))) {
    if (!/^[a-z][a-z0-9]*$/.test(name)) fail(`prefs.json ${g}.${name}: lowercase name`);
    if (isObj(f)) checkPrefField(f, `prefs.json ${g}.${name}`, true);
  }
}

// The panel's full view sections, its corner list and the opacity floor are read from the spec by statsOverlay.ts, not copied.
const overlaySrc = readFileSync(STATS_OVERLAY, "utf8");
for (const [what, re] of [["SECTIONS", /SECTIONS = FIELDS\.folded!\.values/], ["CORNERS", /CORNERS = FIELDS\.corner!\.values/], ["OPACITY_MIN", /OPACITY_MIN = FIELDS\.opacity!\.min!/]] as const) {
  if (!re.test(overlaySrc)) fail(`statsOverlay.ts: ${what} must be read from prefs.json (stats_panel), not written out`);
}
for (const f of ["statsOverlay.ts", "toolbarPrefs.ts"]) {
  const src = readFileSync(join(PORTAL_SRC, f), "utf8");
  if (!/parsePrefsText\(/.test(src)) fail(`${f}: must parse through parsePrefsText (prefs.json)`);
}

const prefCaseFile = JSON.parse(readFileSync(PREFS_CASES, "utf8")) as { defaults: Record<string, Obj>; cases: Obj[] };
const prefCases = prefCaseFile.cases;
// The cases pin each group's defaults themselves (so a changed default fails them); they must agree with the spec.
for (const g of PREF_GROUPS) {
  const pinned = prefCaseFile.defaults?.[g];
  const want = Object.fromEntries(Object.entries(prefFieldsOf(g)).filter(([, f]) => "default" in f).map(([n, f]) => [n, f.default]));
  if (!isObj(pinned)) fail(`prefs-cases.json: defaults.${g} is an object`);
  else if (JSON.stringify(Object.entries(pinned).sort()) !== JSON.stringify(Object.entries(want).sort())) {
    fail(`prefs-cases.json: defaults.${g} is ${JSON.stringify(pinned)}, prefs.json's are ${JSON.stringify(want)}`);
  }
}
const prefNames = new Set<string>();
/** field -> { accepted: a case parsed it to something other than its default; rejected: a case gave it a value that fell back } */
const prefCover = new Map<string, { accepted: boolean; rejected: boolean }>();
for (const g of PREF_GROUPS) for (const n of Object.keys(prefFieldsOf(g))) prefCover.set(`${g}.${n}`, { accepted: false, rejected: false });
for (const c of prefCases) {
  const at = `prefs-cases.json "${c.name}"`;
  const key = `${c.group}/${c.name}`;
  if (typeof c.name !== "string" || prefNames.has(key)) fail(`${at}: names are unique strings within a group`);
  prefNames.add(key);
  const fields = prefFieldsOf(String(c.group));
  if (!PREF_GROUPS.includes(c.group as string)) {
    fail(`${at}: group is one of ${PREF_GROUPS.join(", ")}`);
    continue;
  }
  if (c.platforms !== undefined && (!Array.isArray(c.platforms) || !c.platforms.length || c.platforms.some((p) => !PLATFORMS.includes(p as string)))) fail(`${at}: platforms`);
  if (("text" in c) === ("value" in c)) fail(`${at}: exactly one of text and value`);
  if ("text" in c && c.text !== null && typeof c.text !== "string") fail(`${at}: text is a string or null`);
  if ("text" in c && c.text === null && !(Array.isArray(c.platforms) && c.platforms.join() === "web")) fail(`${at}: nothing saved (text null) only exists on web`);
  if (!isObj(c.expect)) {
    fail(`${at}: expect is an object`);
    continue;
  }
  for (const [name, v] of Object.entries(c.expect)) {
    const f = fields[name];
    if (!f) fail(`${at}: expect "${name}" is not a field of ${c.group}`);
    else {
      const p = prefProblem(f, v);
      if (p) fail(`${at}: expect ${name} = ${JSON.stringify(v)} is ${p}`);
      else if (!("default" in f) || JSON.stringify(f.default) !== JSON.stringify(v)) prefCover.get(`${c.group}.${name}`)!.accepted ||= true;
    }
  }
  // A field the input names but the result lacks (or holds as its default) is one that was rejected or ignored.
  const input = "value" in c ? c.value : (() => { try { return JSON.parse(String(c.text)); } catch { return null; } })();
  if (isObj(input)) {
    for (const name of Object.keys(fields)) {
      if (name in input && !(name in c.expect)) prefCover.get(`${c.group}.${name}`)!.rejected ||= true;
    }
  }
}
for (const [k, v] of prefCover) {
  if (!v.accepted) fail(`prefs-cases.json: no case saves ${k} and sees it kept`);
  if (!v.rejected) fail(`prefs-cases.json: no case gives ${k} a value that falls back`);
}
if (prefCases.length < 20) fail("prefs-cases.json: at least 20 cases");


// --- toolbar.json and its cases ---

const toolbar = JSON.parse(readFileSync(TOOLBAR, "utf8")) as Obj;
const toolbarCases = JSON.parse(readFileSync(TOOLBAR_CASES, "utf8")) as Obj;
const TB_FIELDS = ["icon", "label", "aria_label", "tooltip", "tone", "disabled", "read_only", "aria_text"];
const TB_TONES = ["none", "ok", "accent", "warn", "danger", "dim", "faint"];
const TB_CONTROL_KINDS = ["button", "toggle", "menu", "label", "badge", "list"];
const TB_ROW_KINDS = ["choice", "button", "slider", "list", "note", "link"];
const TB_STATE_KINDS = ["bool", "number", "string", "enum", "list"];
const TB_CONTROL_KEYS = ["id", "kind", "group", "platforms", "needs", "when", "droppable", "hover_tone", "pressed", "states", "badge", "menu", "from", "item_action", ...TB_FIELDS];
const TB_ROW_KEYS = ["id", "kind", "platforms", "needs", "lacks", "when", "states", "group", "dom_id", "role", "text", "value", "options", "edit_needs", "zero_when", "min", "max", "step", "display", "from", "item_detail", "item_detail_none", "to", ...TB_FIELDS];
const TB_VARIANT_KEYS = ["when", "platforms", ...TB_FIELDS];
const TB_OPTION_KEYS = ["value", "platforms", "when", "states", ...TB_FIELDS];
const tbState = (isObj(toolbar.state) ? toolbar.state : {}) as Record<string, Obj>;
const tbCaps = (isObj(toolbar.capabilities) ? toolbar.capabilities : {}) as Record<string, Obj>;
const tbReports = (isObj(toolbar.reports) ? toolbar.reports : {}) as Record<string, string[]>;
const tbControls = (Array.isArray(toolbar.controls) ? toolbar.controls : []) as Obj[];
const tbIds = new Set<string>();
const tbUsedCaps = new Set<string>();
let tbTemplates = 0;
let tbRows = 0;

const unknownKeys = (o: Obj, allowed: string[], at: string) => {
  for (const k of Object.keys(o)) if (!allowed.includes(k)) fail(`${at}: unknown key "${k}"`);
};
const isPlatforms = (v: unknown): v is string[] => Array.isArray(v) && v.length > 0 && v.every((p) => PLATFORMS.includes(p as string));
/** A list's entries must be unique strings. */
const stringList = (v: unknown, at: string): string[] => {
  if (v === undefined) return [];
  if (!Array.isArray(v) || v.some((x) => typeof x !== "string")) {
    fail(`${at}: a list of strings`);
    return [];
  }
  return v as string[];
};

for (const [k, v] of Object.entries(tbState)) {
  const at = `toolbar.json state "${k}"`;
  if (!/^[a-z][a-z0-9_]*$/.test(k)) fail(`${at}: snake_case`);
  if (!TB_STATE_KINDS.includes(v.kind as string)) fail(`${at}: kind is one of ${TB_STATE_KINDS.join(", ")}`);
  if (typeof v.doc !== "string" || !v.doc) fail(`${at}: needs a doc line`);
  if (v.kind === "enum" && !(Array.isArray(v.values) && v.values.length)) fail(`${at}: an enum lists its values`);
  if (v.derived === true) {
    if (v.platforms !== undefined) fail(`${at}: a derived key has no platforms (the model makes it)`);
  } else if (!isPlatforms(v.platforms)) fail(`${at}: platforms is a non-empty list of ${PLATFORMS.join(", ")}`);
}
const stateFilledOn = (key: string, platform: string): boolean => {
  const v = tbState[key];
  return !!v && (v.derived === true || (Array.isArray(v.platforms) && (v.platforms as string[]).includes(platform)));
};

/** A condition: `key`, `!key`, `key=value`, `key!=value` or `key>number`, over a declared state key. */
function checkCond(cond: unknown, platforms: string[], at: string) {
  if (typeof cond !== "string") return fail(`${at}: a condition is a string`);
  let key: string;
  const m = /^([a-z][a-z0-9_]*)(!=|=|>)(.+)$/.exec(cond);
  if (m) {
    key = m[1]!;
    const spec = tbState[key];
    if (spec && m[2] === ">" && (spec.kind !== "number" || !Number.isFinite(Number(m[3])))) fail(`${at}: "${cond}" compares a number key with a number`);
    if (spec && m[2] !== ">" && spec.kind === "enum" && !(spec.values as string[]).includes(m[3]!)) fail(`${at}: "${cond}": "${m[3]}" is not one of ${key}'s values`);
  } else {
    const n = /^!?([a-z][a-z0-9_]*)$/.exec(cond);
    if (!n) return fail(`${at}: "${cond}" is not key, !key, key=value, key!=value or key>number`);
    key = n[1]!;
  }
  if (!tbState[key]) return fail(`${at}: "${key}" is not a declared state key`);
  // A key the platform never fills is simply absent there (a web-only variant on a shared control is fine), but a
  // condition that needs a key none of its platforms fills can never hold.
  const positive = !cond.startsWith("!") && !cond.includes("!=");
  if (positive && platforms.length && !platforms.some((p) => stateFilledOn(key, p))) fail(`${at}: "${cond}" can never hold: ${key} is not filled on ${platforms.join(" or ")}`);
}
const checkConds = (conds: unknown, platforms: string[], at: string) => {
  for (const c of stringList(conds, at)) checkCond(c, platforms, at);
};

/** Every placeholder in a template is a declared state key (or one of the context's own), of the right kind. */
function checkTbTemplate(text: string, platforms: string[], at: string, extra: Record<string, "number" | "string"> = {}) {
  tbTemplates++;
  for (const m of text.matchAll(PLACEHOLDER)) {
    const [, name, fmt] = m as unknown as [string, string, string | undefined];
    const own = extra[name!];
    const spec = tbState[name!];
    const kind = own ?? (spec ? (spec.kind === "number" ? "number" : "string") : undefined);
    if (!kind) {
      fail(`${at}: {${name}} is not a declared state key`);
      continue;
    }
    if (fmt && !FORMATTERS.includes(fmt)) fail(`${at}: unknown formatter "${fmt}"`);
    if (kind === "number" && !fmt) fail(`${at}: {${name}} is a number and needs a formatter`);
    if (kind === "string" && fmt) fail(`${at}: {${name}:${fmt}} is a string and takes no formatter`);
    if (!own) for (const p of platforms) if (!stateFilledOn(name!, p)) fail(`${at}: {${name}} is not filled on ${p}`);
  }
  if (/[{}]/.test(text.replace(PLACEHOLDER, ""))) fail(`${at}: a stray brace in ${JSON.stringify(text)}`);
}

/** A text: a template, or { web?, native? } of templates, shown on `platforms`. */
function checkTbText(t: unknown, platforms: string[], at: string, extra: Record<string, "number" | "string"> = {}) {
  if (typeof t === "string") return checkTbTemplate(t, platforms, at, extra);
  if (!isObj(t)) return fail(`${at}: a string or { web?, native? }`);
  for (const [p, text] of Object.entries(t)) {
    if (!PLATFORMS.includes(p) || typeof text !== "string") fail(`${at}: "${p}" must be web or native with a string`);
    else checkTbTemplate(text, platforms.filter((x) => x === p), `${at}.${p}`, extra);
  }
}

const checkIcon = (id: unknown, at: string) => {
  if (typeof id !== "string" || !Object.hasOwn(icons, id)) fail(`${at}: icon "${String(id)}" is not in icons.json`);
};

function useCaps(list: unknown, at: string) {
  for (const c of stringList(list, at)) {
    if (!tbCaps[c]) fail(`${at}: capability "${c}" is not declared`);
    tbUsedCaps.add(c);
  }
}

/** The fields shared by a control, row, option and variant. */
function checkFields(o: Obj, platforms: string[], at: string, extra: Record<string, "number" | "string"> = {}) {
  if (o.icon !== undefined) checkIcon(o.icon, at);
  for (const f of ["label", "aria_label", "tooltip", "read_only", "aria_text"]) if (o[f] !== undefined) checkTbText(o[f], platforms, `${at} ${f}`, extra);
  if (o.tone !== undefined && !TB_TONES.includes(o.tone as string)) fail(`${at}: tone is one of ${TB_TONES.join(", ")}`);
  if (o.disabled !== undefined && o.disabled !== true) fail(`${at}: disabled is true`);
}

function checkVariants(states: unknown, platforms: string[], at: string, extra: Record<string, "number" | "string"> = {}) {
  if (states === undefined) return;
  if (!Array.isArray(states)) return fail(`${at}: states is a list`);
  (states as Obj[]).forEach((v, n) => {
    const a = `${at} states[${n}]`;
    if (!isObj(v)) return fail(`${a}: an object`);
    unknownKeys(v, TB_VARIANT_KEYS, a);
    if (!Array.isArray(v.when) || !v.when.length) fail(`${a}: needs when`);
    let ps = platforms;
    if (v.platforms !== undefined) {
      if (!isPlatforms(v.platforms)) fail(`${a}: platforms`);
      else ps = platforms.filter((p) => (v.platforms as string[]).includes(p));
    }
    checkConds(v.when, ps, `${a} when`);
    checkFields(v, ps, a, extra);
  });
}

function checkRow(r: Obj, menuId: string, platforms: string[], at: string) {
  tbRows++;
  const id = String(r.id);
  unknownKeys(r, TB_ROW_KEYS, at);
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(id)) fail(`${at}: ids are kebab-case`);
  if (tbIds.has(id)) fail(`${at}: id appears twice`);
  tbIds.add(id);
  if (!TB_ROW_KINDS.includes(r.kind as string)) return fail(`${at}: kind is one of ${TB_ROW_KINDS.join(", ")}`);
  let ps = platforms;
  if (r.platforms !== undefined) {
    if (!isPlatforms(r.platforms)) return fail(`${at}: platforms`);
    ps = platforms.filter((p) => (r.platforms as string[]).includes(p));
    if (!ps.length) fail(`${at}: platforms leave the row nowhere to show (its control is on ${platforms.join(", ")})`);
  }
  useCaps(r.needs, `${at} needs`);
  useCaps(r.lacks, `${at} lacks`);
  useCaps(r.edit_needs, `${at} edit_needs`);
  checkConds(r.when, ps, `${at} when`);
  const extra: Record<string, "number" | "string"> = r.kind === "slider" ? { shown: "number" } : r.kind === "list" ? { n: "number" } : {};
  checkFields(r, ps, at, extra);
  checkVariants(r.states, ps, at, extra);
  if (r.text !== undefined) checkTbText(r.text, ps, `${at} text`);
  const stateKey = (key: unknown, kinds: string[], what: string) => {
    const spec = typeof key === "string" ? tbState[key] : undefined;
    if (!spec) return fail(`${at}: ${what} "${String(key)}" is not a declared state key`);
    if (!kinds.includes(spec.kind as string)) fail(`${at}: ${what} "${String(key)}" is ${spec.kind}, want ${kinds.join(" or ")}`);
    if (!ps.some((p) => stateFilledOn(key as string, p))) fail(`${at}: ${what} "${String(key)}" is not filled on ${ps.join(" or ")}`);
  };
  switch (r.kind) {
    case "choice": {
      stateKey(r.value, ["string", "number", "enum"], "value");
      const o = r.options;
      if (isObj(o) && Array.isArray(o.items)) {
        const values = new Set<string>();
        (o.items as Obj[]).forEach((it, n) => {
          const a = `${at} option[${n}]`;
          unknownKeys(it, TB_OPTION_KEYS, a);
          if (typeof it.value !== "string") fail(`${a}: needs a value`);
          if (values.has(String(it.value)) && !it.when) fail(`${a}: value "${String(it.value)}" appears twice`);
          values.add(String(it.value));
          if (it.label === undefined) fail(`${a}: needs a label`);
          let ops = ps;
          if (it.platforms !== undefined) {
            if (!isPlatforms(it.platforms)) fail(`${a}: platforms`);
            else ops = ps.filter((p) => (it.platforms as string[]).includes(p));
          }
          checkConds(it.when, ops, `${a} when`);
          checkFields(it, ops, a);
          checkVariants(it.states, ops, a);
        });
      } else if (isObj(o) && o.from === "codecs") stateKey("codecs", ["list"], "options.from");
      else fail(`${at}: a choice has options.items or options.from "codecs"`);
      if (r.label === undefined) fail(`${at}: a choice needs a label`);
      break;
    }
    case "slider":
      stateKey(r.value, ["number"], "value");
      for (const k of ["min", "max", "step"]) if (!isNum(r[k])) fail(`${at}: a slider needs ${k}`);
      if (typeof r.display !== "string") fail(`${at}: a slider needs display`);
      else checkTbTemplate(r.display, ps, `${at} display`, { shown: "number" });
      checkConds(r.zero_when, ps, `${at} zero_when`);
      break;
    case "list":
      stateKey(r.from, ["list"], "from");
      break;
    case "note":
      if (r.text === undefined) fail(`${at}: a note needs text`);
      break;
    case "link":
      if (typeof r.to !== "string" || !r.to.startsWith("/")) fail(`${at}: a link needs a path in to`);
      if (r.label === undefined) fail(`${at}: a link needs a label`);
      break;
    case "button":
      if (r.label === undefined) fail(`${at}: a button needs a label`);
      break;
  }
  if (r.dom_id !== undefined && (typeof r.dom_id !== "string" || tbIds.has(`dom:${r.dom_id}`))) fail(`${at}: dom_id must be a unique string`);
  if (typeof r.dom_id === "string") tbIds.add(`dom:${r.dom_id}`);
  void menuId;
}

for (const k of ["fold_after_ms", "near_top_px", "release_hold_ms", "release_hint_ms"]) {
  if (!isNum(toolbar.timing && (toolbar.timing as Obj)[k])) fail(`toolbar.json: timing needs a number ${k}`);
}
{
  const t = (isObj(toolbar.timing) ? toolbar.timing : {}) as Obj;
  if (isNum(t.release_hold_ms) && isNum(t.release_hint_ms) && !(0 < t.release_hint_ms && t.release_hint_ms < t.release_hold_ms)) {
    fail("toolbar.json: timing.release_hint_ms is above 0 and below release_hold_ms");
  }
  checkTbText(toolbar.release_hint, [], "toolbar.json release_hint");
  const ps = isObj(toolbar.release_hint) ? Object.keys(toolbar.release_hint) : [];
  if (ps.length && !(ps.includes("web") && ps.includes("native"))) fail("toolbar.json: release_hint has words for both web and native");
}
{
  const p = (isObj(toolbar.power_off) ? toolbar.power_off : {}) as Obj;
  if (!isNum(p.seconds) || (p.seconds as number) <= 0) fail("toolbar.json: power_off.seconds is a positive number");
  for (const [k, names] of [["title", ["name", "seconds"]], ["stopping", ["name"]], ["failed", ["name"]], ["note", []], ["close", []], ["default_name", []]] as const) {
    if (typeof p[k] !== "string" || !p[k]) fail(`toolbar.json: power_off.${k} is a string`);
    else {
      const extra: Record<string, "number" | "string"> = { name: "string", seconds: "number" };
      for (const m of (p[k] as string).matchAll(PLACEHOLDER)) if (!(names as readonly string[]).includes(m[1]!)) fail(`toolbar.json: power_off.${k} cannot use {${m[1]}}`);
      checkTbTemplate(p[k] as string, [], `toolbar.json power_off.${k}`, extra);
    }
  }
  const cancel = (isObj(p.cancel) ? p.cancel : {}) as Obj;
  if (typeof cancel.label !== "string") fail("toolbar.json: power_off.cancel.label is a string");
  checkTbText(cancel.tooltip, [], "toolbar.json power_off.cancel.tooltip");
}
{
  const f = (isObj(toolbar.folded) ? toolbar.folded : {}) as Obj;
  for (const part of ["bar", "tab"]) {
    const o = (isObj(f[part]) ? f[part] : {}) as Obj;
    for (const k of ["aria_label", "tooltip"]) if (typeof o[k] !== "string" || !o[k]) fail(`toolbar.json: folded.${part}.${k} is a string`);
  }
  checkConds(((f.tab as Obj | undefined)?.disabled), PLATFORMS, "toolbar.json folded.tab.disabled");
  const vis = (isObj(toolbar.visibility) ? toolbar.visibility : {}) as Obj;
  for (const group of ["hide", "show"]) {
    const rules = (Array.isArray(vis[group]) ? vis[group] : []) as Obj[];
    if (!rules.length) fail(`toolbar.json: visibility.${group} needs rules`);
    rules.forEach((r, n) => {
      const ps = r.platforms === undefined ? PLATFORMS : isPlatforms(r.platforms) ? r.platforms : [];
      checkConds(r.when, ps, `toolbar.json visibility.${group}[${n}] when`);
    });
  }
}
for (const [id, c] of Object.entries(tbCaps)) if (typeof c.doc !== "string" || !c.doc) fail(`toolbar.json capability "${id}": needs a doc line`);
for (const p of PLATFORMS) {
  const list = tbReports[p];
  if (!Array.isArray(list)) fail(`toolbar.json: reports.${p} lists the capabilities ${p} can report`);
  else {
    if (new Set(list).size !== list.length) fail(`toolbar.json: reports.${p} lists a capability twice`);
    for (const c of list) if (!tbCaps[c]) fail(`toolbar.json: reports.${p} names "${c}", which is not a declared capability`);
  }
}

const tbControlPlatforms = new Map<string, string[]>();
const tbRowPlatforms = new Map<string, string[]>();
const tbNeeds = new Map<string, string[]>();
for (const c of tbControls) {
  const id = String(c.id);
  const at = `toolbar.json control "${id}"`;
  unknownKeys(c, TB_CONTROL_KEYS, at);
  if (!/^[a-z][a-z0-9]*(-[a-z0-9]+)*$/.test(id)) fail(`${at}: ids are kebab-case`);
  if (tbIds.has(id)) fail(`${at}: id appears twice`);
  tbIds.add(id);
  if (!TB_CONTROL_KINDS.includes(c.kind as string)) fail(`${at}: kind is one of ${TB_CONTROL_KINDS.join(", ")}`);
  if (c.group !== "left" && c.group !== "right") fail(`${at}: group is left or right`);
  let ps = PLATFORMS;
  if (c.platforms !== undefined) {
    if (!isPlatforms(c.platforms)) fail(`${at}: platforms`);
    else ps = c.platforms;
  }
  tbControlPlatforms.set(id, ps);
  tbNeeds.set(id, stringList(c.needs, `${at} needs`));
  useCaps(c.needs, `${at} needs`);
  checkConds(c.when, ps, `${at} when`);
  checkFields(c, ps, at);
  checkVariants(c.states, ps, at);
  if (["button", "toggle", "menu"].includes(c.kind as string) && c.icon === undefined && c.label === undefined) fail(`${at}: a ${c.kind} needs an icon or a label`);
  if (["button", "toggle", "menu"].includes(c.kind as string) && c.icon !== undefined && c.aria_label === undefined) fail(`${at}: an icon button needs an aria_label`);
  if (c.kind === "toggle") {
    if (!Array.isArray(c.pressed) || !c.pressed.length) fail(`${at}: a toggle needs pressed`);
    checkConds(c.pressed, ps, `${at} pressed`);
  } else if (c.pressed !== undefined) fail(`${at}: only a toggle has pressed`);
  if (c.hover_tone !== undefined && !TB_TONES.includes(c.hover_tone as string)) fail(`${at}: hover_tone`);
  if (c.badge !== undefined) {
    const b = (isObj(c.badge) ? c.badge : {}) as Obj;
    unknownKeys(b, ["when", "text", "tone", "tone_from", "tooltip"], `${at} badge`);
    checkConds(b.when, ps, `${at} badge when`);
    if (typeof b.text !== "string") fail(`${at}: badge needs text`);
    else checkTbTemplate(b.text, ps, `${at} badge text`);
    if (b.tooltip !== undefined) checkTbText(b.tooltip, ps, `${at} badge tooltip`);
    if ((b.tone === undefined) === (b.tone_from === undefined)) fail(`${at}: a badge has one of tone and tone_from`);
    if (b.tone !== undefined && !TB_TONES.includes(b.tone as string)) fail(`${at}: badge tone`);
    if (b.tone_from !== undefined && b.tone_from !== "grade") fail(`${at}: tone_from is "grade"`);
  }
  if (c.kind === "menu") {
    const m = (isObj(c.menu) ? c.menu : undefined) as Obj | undefined;
    if (!m) fail(`${at}: a menu control needs a menu`);
    else {
      unknownKeys(m, ["id", "dom_id", "align", "rows"], `${at} menu`);
      if (m.id !== id) fail(`${at}: menu.id is the control's id`);
      if (!((tbState.menu_open?.values as string[] | undefined) ?? []).includes(id)) fail(`${at}: state menu_open does not list "${id}"`);
      if (m.align !== "left" && m.align !== "right") fail(`${at}: menu.align is left or right`);
      if (typeof m.dom_id !== "string" || tbIds.has(`dom:${m.dom_id}`)) fail(`${at}: menu.dom_id must be a unique string`);
      tbIds.add(`dom:${String(m.dom_id)}`);
      (Array.isArray(m.rows) ? (m.rows as Obj[]) : []).forEach((r) => {
        checkRow(r, id, ps, `toolbar.json menu "${id}" row "${String(r.id)}"`);
        const rp = r.platforms === undefined ? ps : ps.filter((p) => (r.platforms as string[]).includes(p));
        tbRowPlatforms.set(String(r.id), rp);
        tbNeeds.set(String(r.id), stringList(r.needs, ""));
      });
    }
  } else if (c.menu !== undefined) fail(`${at}: only a menu control has a menu`);
  if (c.kind === "list") {
    const spec = typeof c.from === "string" ? tbState[c.from] : undefined;
    if (!spec || spec.kind !== "list") fail(`${at}: a list control's from is a declared list state key`);
    const a = (isObj(c.item_action) ? c.item_action : {}) as Obj;
    if (typeof a.label !== "string" || typeof a.tooltip !== "string") fail(`${at}: item_action needs label and tooltip`);
  }
}
for (const [cap] of Object.entries(tbCaps)) if (!tbUsedCaps.has(cap)) fail(`toolbar.json: capability "${cap}" is declared but no control or row needs it`);

// Coverage per platform: a control or row for a platform either can show (every capability it needs is one the platform can
// report) or is gated by one it never reports. Either way it must be sound: a gated one is a spec mistake only if
// nothing on that side could ever show it AND it was meant to (its platforms list says so).
const tbRenderable = { web: [] as string[], native: [] as string[] } as Record<string, string[]>;
const tbGated = { web: [] as string[], native: [] as string[] } as Record<string, string[]>;
for (const [id, ps] of [...tbControlPlatforms, ...tbRowPlatforms]) {
  for (const p of ps) {
    const reported = tbReports[p] ?? [];
    const needs = tbNeeds.get(id) ?? [];
    (needs.every((n) => reported.includes(n)) ? tbRenderable : tbGated)[p]!.push(id);
  }
}
for (const [id, ps] of tbRowPlatforms) if (!ps.length) fail(`toolbar.json row "${id}" shows on no platform`);

// The cases.
const tbCaseList = (Array.isArray(toolbarCases.cases) ? toolbarCases.cases : []) as Obj[];
const tbCaseCaps = (isObj(toolbarCases.caps) ? toolbarCases.caps : {}) as Record<string, string[]>;
const tbDefaults = (isObj(toolbarCases.defaults) ? toolbarCases.defaults : {}) as Record<string, Obj>;
const PIN_FIELDS = ["id", "kind", "group", "droppable", "icon", "label", "aria_label", "tooltip", "pressed", "active", "disabled", "tone", "hover_tone", "badge", "menu", "menu_dom_id", "items"];
const PIN_ROW_FIELDS = ["id", "kind", "group", "dom_id", "role", "label", "tooltip", "text", "tone", "disabled", "value", "options", "editable", "read_only", "slider", "items", "to"];
const tbSeenControls = new Map<string, Set<string>>([["web", new Set()], ["native", new Set()]]);
const tbSeenRows = new Map<string, Set<string>>([["web", new Set()], ["native", new Set()]]);
const capsShown = new Set<string>();
const capsHidden = new Set<string>();

const checkStateValue = (key: string, v: unknown, at: string) => {
  const spec = tbState[key];
  if (!spec) return fail(`${at}: "${key}" is not a declared state key`);
  if (spec.derived === true) return fail(`${at}: "${key}" is derived, the model makes it`);
  const kind = spec.kind;
  if (v === null) return;
  const ok =
    kind === "bool" ? typeof v === "boolean"
    : kind === "number" ? isNum(v)
    : kind === "string" ? typeof v === "string"
    : kind === "enum" ? typeof v === "string" && (spec.values as string[]).includes(v)
    : Array.isArray(v);
  if (!ok) fail(`${at}: ${key} = ${JSON.stringify(v)} is not a ${kind}${kind === "enum" ? ` of ${(spec.values as string[]).join(", ")}` : ""}`);
};
for (const [group, st] of Object.entries(tbDefaults)) {
  if (!["state", "web", "native"].includes(group)) fail(`toolbar-cases.json: defaults.${group} is state, web or native`);
  for (const [k, v] of Object.entries(isObj(st) ? st : {})) {
    checkStateValue(k, v, `toolbar-cases.json defaults.${group}`);
    if (group !== "state" && tbState[k] && !(tbState[k]!.platforms as string[] | undefined)?.includes(group)) fail(`toolbar-cases.json defaults.${group}: "${k}" is not filled on ${group}`);
  }
}
for (const [name, list] of Object.entries(tbCaseCaps)) for (const c of list) if (!tbCaps[c]) fail(`toolbar-cases.json caps "${name}": "${c}" is not declared`);

const tbNames = new Set<string>();
for (const c of tbCaseList) {
  const at = `toolbar-cases.json "${String(c.name)}"`;
  unknownKeys(c, ["name", "platforms", "caps", "state", "expect"], at);
  if (typeof c.name !== "string" || !c.name || tbNames.has(c.name)) fail(`${at}: names are unique`);
  tbNames.add(String(c.name));
  const ps = c.platforms === undefined ? PLATFORMS : isPlatforms(c.platforms) ? c.platforms : (fail(`${at}: platforms`), []);
  const caps = typeof c.caps === "string" ? tbCaseCaps[c.caps] : c.caps;
  if (!Array.isArray(caps)) fail(`${at}: caps is a name from caps or a list`);
  else for (const cap of caps as string[]) if (!tbCaps[cap]) fail(`${at}: capability "${cap}" is not declared`);
  for (const [k, v] of Object.entries(isObj(c.state) ? c.state : {})) {
    checkStateValue(k, v, `${at} state`);
  }
  const expect = c.expect as Obj;
  const per: [string, Obj][] = isObj(expect) && "visible" in expect ? ps.map((p) => [p, expect] as [string, Obj]) : PLATFORMS.filter((p) => ps.includes(p)).map((p) => [p, (expect as Obj)[p] as Obj] as [string, Obj]);
  for (const [p, e] of per) {
    const a = `${at} (${p})`;
    if (!isObj(e)) {
      fail(`${a}: no expectation`);
      continue;
    }
    unknownKeys(e, ["visible", "controls", "pin", "menu", "countdown", "folded_bar", "hide_tab"], a);
    if (typeof e.visible !== "boolean") fail(`${a}: visible is true or false`);
    const ids = stringList(e.controls, `${a} controls`);
    for (const id of ids) {
      if (!tbControlPlatforms.has(id)) fail(`${a}: control "${id}" is not in toolbar.json`);
      else if (!tbControlPlatforms.get(id)!.includes(p)) fail(`${a}: control "${id}" is not a ${p} control`);
      tbSeenControls.get(p)!.add(id);
    }
    for (const [id, fields] of Object.entries(isObj(e.pin) ? e.pin : {})) {
      if (!ids.includes(id)) fail(`${a}: pin "${id}" is not among the controls shown`);
      for (const f of Object.keys(isObj(fields) ? fields : {})) if (!PIN_FIELDS.includes(f)) fail(`${a}: pin "${id}" has no field "${f}"`);
    }
    if (isObj(e.menu)) {
      const menuControl = tbControls.find((x) => x.id === (e.menu as Obj).control);
      if (!menuControl || menuControl.kind !== "menu") fail(`${a}: menu.control "${String((e.menu as Obj).control)}" is not a menu control`);
      for (const id of stringList((e.menu as Obj).rows, `${a} menu rows`)) {
        if (!tbRowPlatforms.has(id)) fail(`${a}: row "${id}" is not in toolbar.json`);
        else if (!tbRowPlatforms.get(id)!.includes(p)) fail(`${a}: row "${id}" is not a ${p} row`);
        tbSeenRows.get(p)!.add(id);
      }
      for (const [id, fields] of Object.entries(isObj((e.menu as Obj).pin) ? ((e.menu as Obj).pin as Obj) : {})) {
        if (!stringList((e.menu as Obj).rows, "").includes(id)) fail(`${a}: menu pin "${id}" is not among the rows shown`);
        for (const f of Object.keys(isObj(fields) ? fields : {})) if (!PIN_ROW_FIELDS.includes(f)) fail(`${a}: menu pin "${id}" has no field "${f}"`);
      }
    }
  }
  if (Array.isArray(caps)) {
    // A capability counts as shown when a case reports it and hidden when one does not.
    for (const cap of Object.keys(tbCaps)) (caps.includes(cap) ? capsShown : capsHidden).add(cap);
  }
}
for (const p of PLATFORMS) {
  for (const id of tbRenderable[p]!) {
    if (tbControlPlatforms.has(id) && !tbSeenControls.get(p)!.has(id)) fail(`toolbar-cases.json: no case shows the control "${id}" on ${p}`);
    if (tbRowPlatforms.has(id) && !tbSeenRows.get(p)!.has(id)) fail(`toolbar-cases.json: no case shows the row "${id}" on ${p}`);
  }
  for (const id of tbGated[p]!) {
    if (tbSeenControls.get(p)!.has(id) || tbSeenRows.get(p)!.has(id)) fail(`toolbar-cases.json: "${id}" needs a capability ${p} never reports, yet a ${p} case shows it`);
  }
}
for (const cap of Object.keys(tbCaps)) {
  if (!capsShown.has(cap)) fail(`toolbar-cases.json: no case reports the capability "${cap}"`);
  if (!capsHidden.has(cap)) fail(`toolbar-cases.json: no case leaves out the capability "${cap}"`);
}
if (tbCaseList.length < 20) fail("toolbar-cases.json: at least 20 cases");

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
    // The toolbar draws the icons toolbar.json names (checked above), through iconName(control).
    if (!ids.length && dyn && /^iconName\([a-z]+\)$/.test(dyn[1]!)) continue;
    if (!ids.length) fail(`${rel}: <Icon> without a readable name`);
    for (const id of ids) {
      used++;
      if (!Object.hasOwn(icons, id)) fail(`${rel}: <Icon> uses "${id}", which icons.json does not have`);
    }
  }
}

// --- capture-cases.json: the hold-Esc release ---
const captureCases = JSON.parse(readFileSync(CAPTURE_CASES, "utf8")) as Obj;
const escEvents = ["down", "repeat", "up", "blur", "tick", "uncapture"];
{
  const at = "capture-cases.json";
  unknownKeys(captureCases, ["notes", "timing", "cases"], at);
  const t = (isObj(captureCases.timing) ? captureCases.timing : {}) as Obj;
  const spec = (isObj(toolbar.timing) ? toolbar.timing : {}) as Obj;
  for (const k of ["release_hold_ms", "release_hint_ms"]) {
    if (t[k] !== spec[k]) fail(`${at}: timing.${k} is ${String(t[k])}, toolbar.json says ${String(spec[k])} (the cases were worked out with the first)`);
  }
}
const escCaseList = (Array.isArray(captureCases.cases) ? captureCases.cases : []) as Obj[];
if (escCaseList.length < 10) fail(`capture-cases.json: at least 10 cases (has ${escCaseList.length})`);
const escNames = new Set<string>();
const escSeenEvents = new Set<string>();
for (const c of escCaseList) {
  const at = `capture-cases.json "${String(c.name)}"`;
  unknownKeys(c, ["name", "steps"], at);
  if (typeof c.name !== "string" || !c.name || escNames.has(c.name)) fail(`${at}: names are unique`);
  escNames.add(String(c.name));
  const steps = (Array.isArray(c.steps) ? c.steps : []) as Obj[];
  if (!steps.length) fail(`${at}: needs steps`);
  let last = -1;
  steps.forEach((st, n) => {
    const a = `${at} step ${n}`;
    unknownKeys(st, ["t", "ev", "captured", "host", "release", "hint"], a);
    if (!isNum(st.t) || st.t < last) fail(`${a}: t is a number, not before the step above`);
    else last = st.t;
    if (typeof st.ev !== "string" || !escEvents.includes(st.ev)) fail(`${a}: ev is one of ${escEvents.join(", ")}`);
    else escSeenEvents.add(st.ev);
    if (st.captured !== undefined && typeof st.captured !== "boolean") fail(`${a}: captured is true or false`);
    if (st.host !== undefined && st.host !== "down" && st.host !== "up") fail(`${a}: host is down or up`);
    if (st.release !== undefined && st.release !== true) fail(`${a}: release is true when set`);
    if (st.hint !== undefined && !(isNum(st.hint) && st.hint > 0 && st.hint <= 1)) fail(`${a}: hint is a progress above 0, up to 1`);
  });
}
for (const ev of escEvents) if (!escSeenEvents.has(ev)) fail(`capture-cases.json: no case uses the event "${ev}"`);

if (errors.length) {
  for (const e of errors) console.error(e);
  console.error(`ui-spec check failed: ${errors.length} problem(s)`);
  process.exit(1);
}
console.log(
  `ui-spec ok: ${Object.keys(icons).length} icons, ${used} uses in the portal; ${issueSpecs.length} health issues ` +
    `(${placeholderUses} placeholders), ${cases.length} health cases (${caseCount.shared} shared, ${caseCount.web} web-only, ${caseCount.native} native-only), ${fillCases.length} fill cases; ` +
    `saved settings: ${[...prefCover.keys()].length} fields, ${prefCases.length} cases; stats panel: ${seenSections.size} sections, ${panelRows} rows, ${panelTemplates} templates, ${panelCases.length} cases, ${formatNames.size} format cases; ` +
    `toolbar: ${tbControls.length} controls, ${tbRows} menu rows, ${Object.keys(tbCaps).length} capabilities, ${tbTemplates} templates, ${tbCaseList.length} cases; hold-Esc: ${escCaseList.length} cases`,
);
