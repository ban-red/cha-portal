// The stats panel's shared cases (web/packages/ui-spec/stats-panel-cases.json, format-cases.json)
// run against the browser's side, plus what a case can't say: that the browser fills the spec's keys.
import { describe, expect, test } from "bun:test";

import type { StatsSnapshot } from "@cha/player";
import { buildPanel, fillPanel, STATS_PANEL } from "@cha/ui-spec";
import { caseValues, describePanel, expectedFor, FORMAT_CASES, PANEL_CASES, runsOn, SMOOTH } from "@cha/ui-spec/panel-cases";

import { codecTag, num } from "./statsOverlay";
import { statsValues } from "./statsValues";

describe("stats panel cases", () => {
  test("there are enough of them", () => {
    expect(PANEL_CASES.length).toBeGreaterThanOrEqual(15);
    expect(new Set(PANEL_CASES.map((c) => c.name)).size).toBe(PANEL_CASES.length);
  });
  // Both platforms' panels come from the same spec and function, so both run here (and in cha-ui-spec).
  for (const platform of ["web", "native"] as const) {
    for (const c of PANEL_CASES.filter((c) => runsOn(c, platform))) {
      test(`${c.name} (${platform})`, () => {
        const want = expectedFor(c, platform);
        const panel = buildPanel(STATS_PANEL, caseValues(c.values), c.health ?? SMOOTH, platform);
        expect(describePanel(panel, want, c)).toEqual(want);
      });
    }
  }
});

describe("format cases", () => {
  for (const c of FORMAT_CASES.fill) {
    test(c.name, () => {
      expect(fillPanel(c.template, caseValues(c.values))).toBe(c.expect);
    });
  }
  for (const c of FORMAT_CASES.codec_tag.filter((c) => runsOn(c, "web"))) {
    test(`codecTag: ${c.name}`, () => {
      expect(codecTag(c.codec, c.transport)).toBe(c.expect);
    });
  }
  test("num agrees with the panel's formatters", () => {
    expect(num(null)).toBe("–");
    expect(num(undefined, 0)).toBe("–");
    expect(num(Number.NaN)).toBe("–");
    expect(num(Number.POSITIVE_INFINITY)).toBe("–");
    expect(num(18.66)).toBe("18.7");
    expect(num(59.5, 0)).toBe("60");
    expect(num(1.005, 2)).toBe(fillPanel("{v:f2}", { v: 1.005 }));
  });
  test("a number without a formatter, or a string with one, is a spec bug", () => {
    expect(() => fillPanel("{a}", { a: 1 })).toThrow();
    expect(() => fillPanel("{a:f0}", { a: "x" })).toThrow();
    expect(() => fillPanel("{a:nope}", {})).toThrow();
  });
});

const FULL: StatsSnapshot = {
  codec: "HEVC · WebTransport",
  width: 2560,
  height: 1440,
  fps: 60,
  targetFps: 60,
  sentFps: 60,
  shownSentFps: 60,
  mbps: 58,
  decodeMs: 3,
  jitterMs: 1,
  rttMs: 2,
  packetsLost: 1,
  framesRecovered: 2,
  framesPartial: 3,
  audioOutUnderruns: 4,
  framesDropped: 5,
  latencyMs: 12,
  deliveryMs: 6,
  deliveryP95Ms: 9,
  frameGapMs: 20,
  audioJitterMs: 30,
  node: {
    cpu: 30,
    cores: 16,
    load1: 2,
    memUsed: 8,
    memTotal: 32,
    gpu: 50,
    vramUsed: 4,
    vramTotal: 12,
    enc: 20,
    dec: 1,
    temp: 70,
    power: 188,
    powerLimit: 320,
    clock: 1980,
    streamerCpu: 40,
    ageMs: 0,
  },
};

describe("the browser fills the panel's values", () => {
  test("every key the spec gives the browser is filled, and nothing else", () => {
    const filled = Object.entries(statsValues({ stats: FULL, codec: "hevc", transport: "webtransport", reconnects: 1 }))
      .filter(([, v]) => v !== undefined)
      .map(([k]) => k)
      .sort();
    const wanted = Object.entries(STATS_PANEL.values)
      .filter(([, v]) => v.platforms.includes("web") && !v.derived)
      .map(([k]) => k)
      .sort();
    expect(filled).toEqual(wanted);
  });
  test("a stream with no stats yet still has counts", () => {
    const v = statsValues({ stats: null, codec: "hevc", transport: "webtransport" });
    expect(v.lost).toBe(0);
    expect(v.dropped).toBe(0);
    expect(v.codec_tag).toBe("HEVC/WT");
    expect(v.shown_fps).toBeUndefined();
  });
  test("what the track path doesn't count is left out", () => {
    const v = statsValues({ stats: { ...FULL, audioOutUnderruns: undefined, framesPartial: 0, codec: "H264", node: null }, codec: "h264", transport: "webrtc" });
    expect(v.recovered).toBeUndefined();
    expect(v.audio_underruns).toBeUndefined();
    expect(v.partial).toBeUndefined();
    expect(v.node_cpu).toBeUndefined();
  });
  test("PyroWave counts partial frames even at zero", () => {
    const v = statsValues({ stats: { ...FULL, codec: "PYROWAVE444 · WebTransport", framesPartial: 0 }, codec: "pyrowave444", transport: "webtransport" });
    expect(v.partial).toBe(0);
  });
  test("a node without cores, a power limit or a card keeps the rest", () => {
    const v = statsValues({
      stats: { ...FULL, node: { cpu: 5, cores: 0, load1: 0, memUsed: 1, memTotal: 2, streamerCpu: 1, powerLimit: 0, ageMs: 0 } },
      codec: "hevc",
      transport: "webtransport",
    });
    expect(v.node_cores).toBeUndefined();
    expect(v.node_power_limit).toBeUndefined();
    expect(v.node_gpu).toBeUndefined();
    expect(v.node_cpu).toBe(5);
  });
});
