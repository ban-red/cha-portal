import { describe, expect, test } from "bun:test";

import { assessHealth } from "./health";
import { recordingMarkdown, summarizeRecording, type RecordingMeta } from "./recording";
import type { StatsSnapshot } from "./stats";

function snap(over: Partial<StatsSnapshot> = {}): StatsSnapshot {
  return {
    codec: "HEVC · WebTransport",
    width: 2560,
    height: 1440,
    fps: 60,
    targetFps: 60,
    sentFps: 60,
    shownSentFps: null,
    mbps: 50,
    decodeMs: 3,
    jitterMs: null,
    rttMs: 2,
    packetsLost: 0,
    framesRecovered: 0,
    framesPartial: 0,
    framesDropped: 0,
    latencyMs: 12,
    deliveryMs: 6,
    deliveryP95Ms: 9,
    frameGapMs: 20,
    audioJitterMs: 0,
    node: null,
    ...over,
  };
}

const meta: RecordingMeta = {
  userAgent: "Mozilla/5.0 Chrome/150",
  transport: "webtransport",
  environment: "env-1",
  app: "Chrome",
  startedAt: Date.UTC(2026, 9, 6, 12, 0, 0),
  durationS: 30,
};

describe("summarizeRecording", () => {
  test("a steady run summarises to its steady numbers", () => {
    const snaps = Array.from({ length: 30 }, () => snap());
    const health = snaps.map((_, i) => assessHealth(snaps.slice(0, i + 1), { visible: true }));
    const r = summarizeRecording(snaps, health, meta);
    expect(r.timestamp).toBe("2026-10-06T12:00:00.000Z");
    expect(r.codec).toBe("HEVC · WebTransport");
    expect(`${r.width}x${r.height}`).toBe("2560x1440");
    expect(r.fps).toEqual({ target: 60, shownP50: 60, shownMin: 60, sentMean: 60 });
    expect(r.latencyMs).toMatchObject({ p50: 12, p95: 12, p99: 12, source: "per-second p50" });
    expect(r.decodeMs).toEqual({ p50: 3, p95: 3 });
    expect(r.bitrateMbpsAvg).toBe(50);
    expect([r.framesLost, r.framesRecovered, r.framesDropped]).toEqual([0, 0, 0]);
    expect(r.freezes).toEqual({ longestGapMs: 20, count: 0 });
    expect(r.audioBufferMs).toEqual({ mean: 0, max: 0 });
    expect(r.health).toMatchObject({ worstGrade: "A", finalGrade: "A", reasons: [] });
  });

  test("a still screen is no freeze", () => {
    const snaps = Array.from({ length: 30 }, () => snap({ fps: 0, sentFps: 0, frameGapMs: 1000 }));
    const health = snaps.map((_, i) => assessHealth(snaps.slice(0, i + 1), { visible: true }));
    const r = summarizeRecording(snaps, health, meta);
    expect(r.freezes).toEqual({ longestGapMs: null, count: 0 });
    expect(r.fps.shownP50).toBeNull();
  });

  test("per-frame latency samples give the true percentiles", () => {
    const samples = Array.from({ length: 1000 }, (_, i) => i + 1); // 1..1000 ms
    const r = summarizeRecording([snap()], [], { ...meta, latencySamples: samples });
    expect(r.latencyMs).toMatchObject({ p50: 500, p95: 950, p99: 990, samples: 1000, source: "frames" });
  });

  test("counters report their growth, and a reconnect's reset counts what followed", () => {
    const snaps = [5, 7, 10, 2, 4].map((n) => snap({ packetsLost: n, framesDropped: n * 2 }));
    const r = summarizeRecording(snaps, [], meta);
    expect(r.framesLost).toBe(2 + 3 + 2 + 2);
    expect(r.framesDropped).toBe(4 + 6 + 4 + 4);
  });

  test("freezes: the longest gap, and the seconds that passed the threshold", () => {
    const gaps = [20, 20, 300, 20, 900, 40];
    const r = summarizeRecording(gaps.map((g) => snap({ frameGapMs: g })), [], meta);
    expect(r.freezes).toEqual({ longestGapMs: 900, count: 2 });
  });

  test("shown fps: p50 and min; the worst health moment gives the reasons", () => {
    const snaps = [60, 60, 60, 40, 60].map((fps) => snap({ fps }));
    const bad = assessHealth([snap({ fps: 30 }), snap({ fps: 30 }), snap({ fps: 30 })], { visible: true });
    expect(bad.issues.length).toBeGreaterThan(0);
    const good = assessHealth([snap(), snap()], { visible: true });
    const r = summarizeRecording(snaps, [good, bad, good], meta);
    expect(r.fps.shownP50).toBe(60);
    expect(r.fps.shownMin).toBe(40);
    expect(r.health.worstGrade).toBe(bad.grade!);
    expect(r.health.finalGrade).toBe("A");
    expect(r.health.reasons.length).toBe(bad.issues.length);
  });

  test("a snapshot without a field (WebRTC) leaves it out rather than zero", () => {
    const snaps = [snap({ deliveryP95Ms: null, frameGapMs: null, audioJitterMs: null, sentFps: null, targetFps: null })];
    const r = summarizeRecording(snaps, [], { ...meta, transport: "webrtc" });
    expect(r.deliveryP95Ms).toBeNull();
    expect(r.freezes).toEqual({ longestGapMs: null, count: 0 });
    expect(r.audioBufferMs).toEqual({ mean: null, max: null });
    expect(r.fps.sentMean).toBeNull();
    expect(r.fps.target).toBeNull();
  });

  test("an empty run is all nulls, not a crash", () => {
    const r = summarizeRecording([], [], meta);
    expect(r.latencyMs.p50).toBeNull();
    expect(r.snapshots).toBe(0);
    expect(r.health.worstGrade).toBeNull();
    expect(recordingMarkdown(r)).toContain("n/a");
  });

  test("the Markdown has a row per measurement", () => {
    const md = recordingMarkdown(summarizeRecording([snap(), snap()], [], meta));
    expect(md).toContain("| Browser | Mozilla/5.0 Chrome/150 |");
    expect(md).toContain("| Decoded size | 2560×1440 |");
    expect(md).toContain("| Shown at | its own size |");
    expect(md).toContain("p50 12 ms, p95 12 ms, p99 12 ms");
    expect(md).not.toContain("Upscale");
  });

  test("a stretched picture records where it was shown, by what, and the upscaler's time", () => {
    const scaled = (ms: number | null) =>
      snap({ width: 2216, height: 1440, shownWidth: 3022, shownHeight: 1964, upscaler: "SGSR 1", upscaleMs: ms });
    const r = summarizeRecording([scaled(0.8), scaled(null), scaled(1.2), scaled(0.9)], [], meta);
    expect(r.shown).toEqual({ width: 3022, height: 1964, upscaler: "SGSR 1" });
    expect(r.upscaleMs).toEqual({ p50: 0.9, p95: 1.2 });
    const md = recordingMarkdown(r);
    expect(md).toContain("| Shown at | 3022×1964 (SGSR 1) |");
    expect(md).toContain("| Upscale (per-second mean) | p50 0.9 ms, p95 1.2 ms |");
  });

  test("a recording keeps the colour, the screen and how long pictures waited on the node", () => {
    const r = summarizeRecording(
      [
        snap({ colour: "8-bit sRGB, BT.709 primaries", display: "120 Hz, HDR", nodeWaitMs: 8.2 }),
        snap({ colour: "8-bit sRGB, BT.709 primaries", display: "120 Hz, HDR", nodeWaitMs: 9.1 }),
        snap({ colour: "8-bit sRGB, BT.709 primaries", display: "120 Hz, HDR", nodeWaitMs: 1.4 }),
      ],
      [],
      meta,
    );
    expect(r.colour).toBe("8-bit sRGB, BT.709 primaries");
    expect(r.display).toBe("120 Hz, HDR");
    expect(r.nodeWaitMs).toEqual({ p50: 8.2, p95: 9.1 });
    const md = recordingMarkdown(r);
    expect(md).toContain("| Colour | 8-bit sRGB, BT.709 primaries |");
    expect(md).toContain("| Display | 120 Hz, HDR |");
    expect(md).toContain("| Wait on node (streamer's medians) | p50 8.2 ms, p95 9.1 ms |");
    // An older streamer says nothing about the wait: no row for it.
    expect(recordingMarkdown(summarizeRecording([snap()], [], meta))).not.toContain("Wait on node");
  });

  test("a value's pipes and backslashes stay inside its cell", () => {
    const md = recordingMarkdown(summarizeRecording([snap()], [], { ...meta, userAgent: "a|b\\" }));
    expect(md).toContain("| Browser | a\\|b\\\\ |");
  });
});
