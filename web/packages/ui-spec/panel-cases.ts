// The shared stats panel cases (stats-panel-cases.json, format-cases.json), typed, and the helper both
// of this package's runners use. Kept out of `index.ts` so the portal's bundle doesn't pull the cases in.
import formatJson from "./format-cases.json";
import casesJson from "./stats-panel-cases.json";
import type { Panel, PanelHealth, PanelValues, ReportIssue, Tone } from "./panel";
import { panelReport } from "./panel";
import type { Platform } from "./index";

/** A value in a case: a number, a string, null (missing), or `{ "nan": true }` (which reads as missing). */
export type CaseValue = number | string | null | { nan: true };

/** What a case pins about one row. `parts` replaces `value` and `tone` where a row's pieces differ in tone. */
export interface ExpectedRow {
  id: string;
  label: string;
  value?: string;
  tone?: Tone;
  /** [text] or [text, tone]. */
  parts?: ([string] | [string, Tone])[];
}

export interface ExpectedSection {
  id: string;
  summary: string;
  summary_tone?: Tone;
  /** Exactly the rows shown, in order. */
  row_ids: string[];
  /** The rows the case pins, in order (a subset of `row_ids`, all of them in the long cases). */
  rows?: ExpectedRow[];
}

export interface ExpectedPanel {
  grade_tone: Tone;
  compact: string;
  /** Exactly the sections shown, in order. */
  sections: ExpectedSection[];
  /** The whole copy report, for the case's `report_issues` and `agent`. */
  report?: string;
}

export interface PanelCase {
  name: string;
  /** Absent means both. */
  platforms?: Platform[];
  values: Record<string, CaseValue>;
  /** Default: grade A, 100, "Smooth", no issues. */
  health?: PanelHealth;
  report_issues?: ReportIssue[];
  /** Default "Test Agent/1.0". */
  agent?: string;
  /** What both players show, or what each does. */
  expect: ExpectedPanel | { web?: ExpectedPanel; native?: ExpectedPanel };
}

export const PANEL_CASES = casesJson as unknown as PanelCase[];

export const SMOOTH: PanelHealth = { grade: "A", score: 100, summary: "Smooth", issues: [] };
export const DEFAULT_AGENT = "Test Agent/1.0";

export interface FormatCase {
  name: string;
  template: string;
  values: Record<string, CaseValue>;
  expect: string;
}

export interface CodecTagCase {
  name: string;
  platforms?: Platform[];
  codec: string | null;
  transport: "webtransport" | "webrtc" | null;
  expect: string;
}

export const FORMAT_CASES = formatJson as unknown as { fill: FormatCase[]; codec_tag: CodecTagCase[] };

/** A case's values as the panel takes them. */
export function caseValues(values: Record<string, CaseValue>): PanelValues {
  const out: PanelValues = {};
  for (const [k, v] of Object.entries(values)) out[k] = v !== null && typeof v === "object" ? Number.NaN : v;
  return out;
}

export const runsOn = (c: { platforms?: Platform[] }, platform: Platform): boolean => !c.platforms || c.platforms.includes(platform);

/** What a case expects on a platform. */
export function expectedFor(c: PanelCase, platform: Platform): ExpectedPanel {
  const e = c.expect as ExpectedPanel & { web?: ExpectedPanel; native?: ExpectedPanel };
  return "grade_tone" in e ? e : (e[platform] as ExpectedPanel);
}

/** The panel in the shape a case pins: the same sections and row ids, and the rows the case lists, with the report if it has one. */
export function describePanel(panel: Panel, expected: ExpectedPanel, c: PanelCase): ExpectedPanel {
  const sections: ExpectedSection[] = panel.sections.map((s) => {
    const want = expected.sections.find((e) => e.id === s.id);
    const section: ExpectedSection = { id: s.id, summary: s.summary.text, row_ids: s.rows.map((r) => r.id) };
    if (s.summary.tone !== "none") section.summary_tone = s.summary.tone;
    if (want?.rows) {
      section.rows = want.rows.map((w) => {
        const r = s.rows.find((x) => x.id === w.id);
        if (!r) return { id: w.id, label: "(missing)" };
        const row: ExpectedRow = { id: r.id, label: r.label };
        if (r.segments.length > 1) {
          row.parts = r.segments.map((x) => (x.tone === "none" ? [x.text] : [x.text, x.tone]));
          if (r.tone !== "none") row.tone = r.tone;
        } else {
          row.value = r.value;
          if (r.tone !== "none") row.tone = r.tone;
        }
        return row;
      });
    }
    return section;
  });
  const out: ExpectedPanel = { grade_tone: panel.gradeTone, compact: panel.compact, sections };
  if (expected.report !== undefined) out.report = panelReport(panel, c.report_issues ?? [], c.agent ?? DEFAULT_AGENT);
  return out;
}
