// The stats panel's spec (stats-panel.json, ADR 0016) and its model: `buildPanel` turns the values a
// player measured into the rows, summaries, compact line and copy report both players draw.
// crates/cha-ui-spec/src/panel.rs is the Rust twin; stats-panel-cases.json keeps them equal.
import panelJson from "./stats-panel.json";
import { FORMATTERS, fixed, PLACEHOLDER, type Platform, type PlatformText, UNITS } from "./index";

/** What a panel colour means; the renderer maps it to a theme colour. `dim` is the secondary ink, `none` the normal ink. */
export type Tone = "none" | "ok" | "warn" | "danger" | "dim";

/** A theme role, named in the spec (a section's heading colour). */
export type SectionColor = "accent" | "chart-1" | "chart-2" | "chart-3";

/** A reading by value key. A key a player doesn't have, can't measure yet or got NaN for is left out, and reads "–". */
export type PanelValues = Record<string, number | string | null | undefined>;

/** The value keys required to be present, for everyone or for a platform (a platform not listed needs none). */
export type Keys = string[] | { web?: string[]; native?: string[] };

/** Which issue ids colour a value, for everyone or for a platform. */
export type BadIds = string[] | { web?: string[]; native?: string[] };

/** Hot: at or over `limit`, in the value, or in percent of `of` (the key holding the total). Never hot when `key` or `of` is missing. */
export interface Hot {
  key: string;
  of?: string;
  limit: number;
}

/** A piece of text with the conditions it shows under; pieces are joined by the spec's rules (rows: nothing; summaries and report lines: their separator). */
export interface Part {
  text: PlatformText;
  /** Shown only if every key is present. */
  when?: Keys;
  /** Shown only if none of the keys is present. */
  unless?: string[];
  platforms?: Platform[];
  /** A row value's pieces: the tone while hot. */
  hot?: Hot;
  /** A row value's pieces: a fixed tone. */
  tone?: "dim";
}

export interface RowSpec {
  id: string;
  label: PlatformText;
  tooltip: PlatformText;
  /** One template, or pieces (each can be hot, dim or conditional). */
  value: string | Part[];
  bad?: BadIds;
  platforms?: Platform[];
  when?: Keys;
}

export interface SectionSpec {
  id: string;
  heading: string;
  color: SectionColor;
  when?: Keys;
  summary: { parts: Part[]; bad?: BadIds; hot?: Hot[] };
  rows: RowSpec[];
}

export interface ReportLineSpec {
  id: string;
  label: string;
  platforms?: Platform[];
  when?: Keys;
  parts: Part[];
}

export type ValueKind = "number" | "string";

export interface StatsPanelSpec {
  separator: string;
  grade_tone: Record<"A" | "B" | "C" | "D" | "F" | "none", Tone>;
  severity_tone: Record<"minor" | "major" | "critical", Tone>;
  hot_tone: Tone;
  /** Every value key a template, `when`, `unless` or `hot` may use. `derived` ones are made from the health grade or the call. */
  values: Record<string, { kind: ValueKind; platforms: Platform[]; derived?: boolean }>;
  compact: Part[];
  sections: SectionSpec[];
  report: { issue: string; lines: ReportLineSpec[]; agent: PlatformText };
}

export const STATS_PANEL = panelJson as unknown as StatsPanelSpec;

/** The health grade as the panel needs it. `HealthAssessment` from `@cha/player` fits. */
export interface PanelHealth {
  grade: string | null;
  score: number | null;
  summary: string;
  issues: { id: string; severity: "minor" | "major" | "critical" }[];
}

export interface PanelSegment {
  text: string;
  tone: Tone;
}

export interface PanelRow {
  id: string;
  label: string;
  tooltip: string;
  /** The tone of the whole value (a segment's own tone, when it has one, wins). */
  tone: Tone;
  /** The value in pieces; neighbours of the same tone are one piece. */
  segments: PanelSegment[];
  /** The pieces' text, joined. */
  value: string;
}

export interface PanelSection {
  id: string;
  heading: string;
  color: SectionColor;
  /** What a folded section shows beside its heading. */
  summary: { text: string; tone: Tone };
  rows: PanelRow[];
}

export interface Panel {
  /** The grade letter's tone in the header and the hidden chip. */
  gradeTone: Tone;
  /** "60 fps · 11.6 ms · 58.2 Mbit/s · HEVC/WT". */
  compact: string;
  /** Only the sections and rows this player shows right now, in order. */
  sections: PanelSection[];
  /** The copy report's numbers, one line each (see `panelReport`). */
  reportLines: string[];
  issueTemplate: string;
  agentTemplate: string;
}

export interface ReportIssue {
  title: string;
  detail: string;
  hint: string;
}

// --- filling ---

const DASH = "–";
type Known = Record<string, number | string>;

/** The values that are really there: numbers that are finite, and strings. */
function known(values: PanelValues): Known {
  const out: Known = {};
  for (const [k, v] of Object.entries(values)) {
    if (typeof v === "string" || (typeof v === "number" && Number.isFinite(v))) out[k] = v;
  }
  return out;
}

/**
 * Fills a template like `fill`, but a value that is missing reads "–" (and its unit: "– ms"),
 * where `fill` throws. A number still needs a formatter and a string takes none.
 */
export function fillPanel(template: string, values: PanelValues): string {
  const have = known(values);
  return template.replace(PLACEHOLDER, (_, name: string, fmt: string | undefined) => {
    const v = have[name];
    if (typeof v === "string") {
      if (fmt) throw new Error(`fillPanel: {${name}:${fmt}} is a string and takes no formatter`);
      return v;
    }
    if (fmt !== undefined && !Object.hasOwn(FORMATTERS, fmt)) throw new Error(`fillPanel: unknown formatter ${fmt} in "${template}"`);
    if (v === undefined) return fmt === "s" ? "" : fmt ? DASH + (UNITS[fmt] ?? "") : DASH;
    if (!fmt) throw new Error(`fillPanel: {${name}} is a number and needs a formatter`);
    return FORMATTERS[fmt]!(v);
  });
}

/** The text for a platform; throws when the spec has none (a spec bug the check catches). */
function text(t: PlatformText, platform: Platform): string {
  const s = typeof t === "string" ? t : t[platform];
  if (s === undefined) throw new Error(`stats-panel.json: no text for ${platform}`);
  return s;
}

const keysFor = (keys: Keys | undefined, platform: Platform): string[] => (keys === undefined ? [] : Array.isArray(keys) ? keys : (keys[platform] ?? []));
const onPlatform = (platforms: Platform[] | undefined, platform: Platform) => !platforms || platforms.includes(platform);

function shown(c: { platforms?: Platform[]; when?: Keys; unless?: string[] }, have: Known, platform: Platform): boolean {
  return (
    onPlatform(c.platforms, platform) &&
    keysFor(c.when, platform).every((k) => k in have) &&
    (c.unless ?? []).every((k) => !(k in have))
  );
}

/** Percent of `total`, 0 when there is none. */
const pct = (used: number, total: number): number => (total > 0 ? (used / total) * 100 : 0);

function isHot(hot: Hot | undefined, have: Known): boolean {
  if (!hot) return false;
  const v = have[hot.key];
  if (typeof v !== "number") return false;
  if (hot.of === undefined) return v >= hot.limit;
  const total = have[hot.of];
  return typeof total === "number" && pct(v, total) >= hot.limit;
}

/** A value coloured only when health found its signal bad, in that issue's severity. */
function badTone(ids: BadIds | undefined, health: PanelHealth, platform: Platform): Tone {
  const list = keysFor(ids, platform);
  const issue = health.issues.find((i) => list.includes(i.id));
  return issue ? STATS_PANEL.severity_tone[issue.severity] : "none";
}

function parts(list: Part[], have: Known, platform: Platform, joiner: string): string {
  return list
    .filter((p) => shown(p, have, platform))
    .map((p) => fillPanel(text(p.text, platform), have))
    .filter((t) => t !== "")
    .join(joiner);
}

function rowSegments(value: string | Part[], have: Known, platform: Platform): PanelSegment[] {
  const list: Part[] = typeof value === "string" ? [{ text: value }] : value;
  const out: PanelSegment[] = [];
  for (const p of list) {
    if (!shown(p, have, platform)) continue;
    const t = fillPanel(text(p.text, platform), have);
    const tone: Tone = p.tone ?? (isHot(p.hot, have) ? STATS_PANEL.hot_tone : "none");
    const last = out[out.length - 1];
    if (last && last.tone === tone) last.text += t;
    else out.push({ text: t, tone });
  }
  return out;
}

/**
 * The panel a player draws, from what it measured. `values` are the spec's value keys; the
 * health-derived ones are added here. Pure: the same input gives the same panel on both players.
 */
export function buildPanel(spec: StatsPanelSpec, values: PanelValues, health: PanelHealth, platform: Platform): Panel {
  const have: Known = known(values);
  if (health.grade !== null) have.health_grade = health.grade;
  else delete have.health_grade;
  if (health.score !== null) have.health_score = health.score;
  else delete have.health_score;
  have.health_summary = health.summary;

  const sections: PanelSection[] = [];
  for (const s of spec.sections) {
    if (!shown({ when: s.when }, have, platform)) continue;
    const hot = (s.summary.hot ?? []).some((h) => isHot(h, have));
    const bad = badTone(s.summary.bad, health, platform);
    const rows: PanelRow[] = [];
    for (const r of s.rows) {
      if (!shown(r, have, platform)) continue;
      let segments = rowSegments(r.value, have, platform);
      let tone = badTone(r.bad, health, platform);
      // A row that is one piece wears its tone whole.
      if (segments.length === 1 && tone === "none") {
        tone = segments[0]!.tone;
        segments = [{ text: segments[0]!.text, tone: "none" }];
      }
      rows.push({ id: r.id, label: text(r.label, platform), tooltip: text(r.tooltip, platform), tone, segments, value: segments.map((x) => x.text).join("") });
    }
    sections.push({
      id: s.id,
      heading: s.heading,
      color: s.color,
      summary: { text: parts(s.summary.parts, have, platform, spec.separator), tone: bad !== "none" ? bad : hot ? spec.hot_tone : "none" },
      rows,
    });
  }

  const reportLines: string[] = [];
  for (const line of spec.report.lines) {
    if (!shown(line, have, platform)) continue;
    const body = parts(line.parts, have, platform, ", ");
    if (body) reportLines.push(`${line.label}: ${body}`);
  }

  return {
    gradeTone: spec.grade_tone[(health.grade as keyof StatsPanelSpec["grade_tone"] | null) ?? "none"],
    compact: parts(spec.compact, have, platform, spec.separator),
    sections,
    reportLines,
    issueTemplate: spec.report.issue,
    agentTemplate: text(spec.report.agent, platform),
  };
}

/**
 * The text a copy button puts on the clipboard: the issues with their hints, then the panel's
 * numbers, then who is asking (`agent`: the browser's user agent, or "Cha Player 0.1.0, macOS 15.5").
 */
export function panelReport(panel: Panel, issues: ReportIssue[], agent: string): string {
  const lines: string[] = [];
  for (const i of issues) {
    lines.push(fillPanel(panel.issueTemplate, { title: i.title, detail: i.detail }), i.hint, "");
  }
  lines.push(...panel.reportLines, fillPanel(panel.agentTemplate, { agent }));
  return lines.join("\n");
}

/** A number with fixed digits, or an en dash when there is none (the stats panel's `num`). */
export function formatNumber(v: number | null | undefined, digits = 1): string {
  return v === null || v === undefined || !Number.isFinite(v) ? DASH : fixed(v, digits);
}
