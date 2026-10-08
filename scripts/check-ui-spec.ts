// Validates the in-stream UI spec (web/packages/ui-spec, ADR 0016) and that the portal only uses
// icons it holds. The native side's coverage is compile-time: crates/cha-ui-spec generates an `Icon`
// enum from icons.json, so a misspelt icon in Rust does not build. For health.json it checks the
// issues (ids, platforms, wording, template placeholders) and health-cases.json (known issue ids,
// severities and fields); the checks' own coverage is in each player's tests.
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
    `(${placeholderUses} placeholders), ${cases.length} health cases (${caseCount.shared} shared, ${caseCount.web} web-only, ${caseCount.native} native-only), ${fillCases.length} fill cases; ` +
    `saved settings: ${[...prefCover.keys()].length} fields, ${prefCases.length} cases; stats panel: ${seenSections.size} sections, ${panelRows} rows, ${panelTemplates} templates, ${panelCases.length} cases, ${formatNames.size} format cases`,
);
