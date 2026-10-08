import { describe, expect, test } from "bun:test";

import { fill, HEALTH, healthIssues } from "@cha/ui-spec";
import fillCases from "@cha/ui-spec/fill-cases.json";
import {
  expandHistory,
  expectedText,
  HEALTH_CASES,
  runsOn,
  type SpecNum,
  type SpecSnapshot,
} from "@cha/ui-spec/health-cases";

import { assessHealth, CHECK_IDS, HEALTH_WINDOW, MIN_SNAPSHOTS } from "./health";
import type { StatsSnapshot } from "./stats";

// The shared cases (web/packages/ui-spec/health-cases.json) run here and in crates/cha-player/src/health.rs.
// What stays in this file is what a case can't say: the mapping, the spec's coverage and the interval.

const num = (v: SpecNum | null | undefined): number | null => (v === undefined || v === null ? null : v === "NaN" ? NaN : v);
const opt = (v: SpecNum | null | undefined): number | undefined => num(v) ?? undefined;

/** A neutral snapshot as the browser's own. Fields the browser can't see in the neutral one (the size) are filled in. */
export function fromSpecSnapshot(s: SpecSnapshot): StatsSnapshot {
  const n = s.node;
  return {
    codec: s.codec ? `${s.codec.toUpperCase()} · WebTransport` : null,
    width: 2560,
    height: 1440,
    fps: num(s.shown_fps),
    targetFps: num(s.target_fps),
    sentFps: num(s.sent_fps),
    shownSentFps: num(s.shown_sent_fps),
    encodeP99Ms: num(s.encode_p99_ms),
    mbps: null,
    decodeMs: num(s.decode_ms),
    jitterMs: num(s.jitter_buffer_ms),
    rttMs: num(s.rtt_ms),
    packetsLost: num(s.lost) ?? 0,
    framesRecovered: num(s.recovered) ?? 0,
    framesPartial: num(s.partial) ?? 0,
    framesDropped: num(s.dropped) ?? 0,
    latencyMs: num(s.latency_ms),
    deliveryMs: num(s.delivery_ms),
    deliveryP95Ms: num(s.delivery_p95_ms),
    frameGapMs: num(s.frame_gap_ms),
    audioJitterMs: num(s.audio_jitter_ms),
    audioRestarts: opt(s.audio_restarts),
    audioInPeak: num(s.audio_in_peak),
    audioOutPeak: num(s.audio_out_peak),
    node: n
      ? {
          cpu: num(n.cpu)!,
          cores: num(n.cores) ?? 0,
          load1: 0,
          memUsed: num(n.mem_used) ?? 0,
          memTotal: num(n.mem_total)!,
          gpu: opt(n.gpu),
          vramUsed: opt(n.vram_used),
          vramTotal: opt(n.vram_total),
          enc: opt(n.enc),
          streamerCpu: num(n.streamer_cpu) ?? 0,
          ageMs: 0,
        }
      : null,
  };
}

describe("the shared health cases", () => {
  const cases = HEALTH_CASES.cases.filter((c) => runsOn(c, "web"));

  test("there are cases to run", () => {
    expect(cases.length).toBeGreaterThan(50);
    expect(new Set(HEALTH_CASES.cases.map((c) => c.name)).size).toBe(HEALTH_CASES.cases.length);
  });

  for (const c of cases) {
    test(c.name, () => {
      const history = expandHistory(HEALTH_CASES.base, c.history).map(fromSpecSnapshot);
      const h = assessHealth(history, { visible: c.context?.visible ?? true, intervalMs: c.context?.interval_ms });
      const e = c.expect;
      const pickTexts = (want: Record<string, Parameters<typeof expectedText>[0]> | undefined, field: "detail" | "hint") =>
        want && Object.fromEntries(Object.keys(want).map((id) => [id, h.issues.find((i) => i.id === id)?.[field]]));
      const wantTexts = (want: Record<string, Parameters<typeof expectedText>[0]> | undefined) =>
        want && Object.fromEntries(Object.entries(want).map(([id, t]) => [id, expectedText(t, "web")]));
      expect({
        grade: h.grade,
        score: h.score,
        summary: h.summary,
        issues: h.issues.map((i) => ({ id: i.id, severity: i.severity })),
        details: pickTexts(e.details, "detail"),
        hints: pickTexts(e.hints, "hint"),
      }).toEqual({
        grade: e.grade,
        score: e.score,
        summary: e.summary,
        issues: e.issues,
        details: wantTexts(e.details),
        hints: wantTexts(e.hints),
      });
    });
  }
});

describe("the spec's coverage", () => {
  test("every web issue in health.json has a check, and every check has an issue", () => {
    const spec = healthIssues("web").map((i) => i.id);
    expect([...CHECK_IDS].sort()).toEqual([...spec].sort());
  });

  test("every web issue has its title and hint for the browser", () => {
    for (const i of healthIssues("web")) {
      expect(typeof i.title === "string" || i.title.web !== undefined).toBe(true);
      expect(typeof i.hint === "string" || i.hint.web !== undefined).toBe(true);
    }
  });

  test("the window and minimum come from the spec", () => {
    expect(HEALTH_WINDOW).toBe(HEALTH.window);
    expect(MIN_SNAPSHOTS).toBe(HEALTH.min_snapshots);
  });
});

describe("fill", () => {
  type V = number | string | { nan: true };
  for (const c of fillCases as { name: string; template: string; values: Record<string, V>; expect: string }[]) {
    test(c.name, () => {
      const values = Object.fromEntries(Object.entries(c.values).map(([k, v]) => [k, typeof v === "object" ? NaN : v]));
      expect(fill(c.template, values)).toBe(c.expect);
    });
  }

  test("a missing value, a number without a formatter and a string with one are errors", () => {
    expect(() => fill("{a}", {})).toThrow();
    expect(() => fill("{a}", { a: 1 })).toThrow();
    expect(() => fill("{a:f0}", { a: "x" })).toThrow();
    expect(() => fill("{a:nope}", { a: 1 })).toThrow();
  });
});

describe("the interval", () => {
  // Cases fix it at one second; the browser's portal can poll at another.
  const second = (over: Partial<SpecSnapshot>) => expandHistory(HEALTH_CASES.base, [{ repeat: 8, ...over }]).map(fromSpecSnapshot);

  test("counter growth is divided by the time between snapshots", () => {
    const history = expandHistory(HEALTH_CASES.base, [{ repeat: 8, recovered: [0, 8, 16, 24, 32, 40, 48, 56] }]).map(fromSpecSnapshot);
    // 8 a second at 1 s is a note; at 8 s a snapshot it is 1 a second, under the band.
    expect(assessHealth(history, { visible: true, intervalMs: 1000 }).issues.map((i) => i.id)).toEqual(["recovered"]);
    expect(assessHealth(history, { visible: true, intervalMs: 8000 }).issues).toEqual([]);
  });

  test("a healthy stream is an A at any interval", () => {
    expect(assessHealth(second({}), { visible: true, intervalMs: 500 }).grade).toBe("A");
  });
});
