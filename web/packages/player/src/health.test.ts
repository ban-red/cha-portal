import { describe, expect, test } from "bun:test";

import { assessHealth, HEALTH_WINDOW, type HealthContext } from "./health";
import type { NodeStats, StatsSnapshot } from "./stats";

const visible: HealthContext = { visible: true };

const healthyNode: NodeStats = {
  cpu: 30,
  cores: 16,
  load1: 2,
  memUsed: 8,
  memTotal: 32,
  gpu: 50,
  vramUsed: 4,
  vramTotal: 12,
  enc: 20,
  dec: 0,
  streamerCpu: 40,
  ageMs: 200,
};

/** A WebTransport snapshot of a healthy 60 fps LAN stream. */
function snap(over: Partial<StatsSnapshot> = {}): StatsSnapshot {
  return {
    codec: "HEVC · WebTransport",
    width: 2560,
    height: 1440,
    fps: 60,
    targetFps: 60,
    sentFps: 60,
    shownSentFps: null,
    mbps: 60,
    decodeMs: 3,
    jitterMs: null,
    rttMs: 2,
    packetsLost: 0,
    framesRecovered: 0,
    framesDropped: 0,
    latencyMs: 12,
    deliveryMs: 6,
    deliveryP95Ms: 9,
    frameGapMs: 20,
    audioJitterMs: null,
    node: healthyNode,
    ...over,
  };
}

const run = (n: number, over: Partial<StatsSnapshot> | ((i: number) => Partial<StatsSnapshot>) = {}) =>
  Array.from({ length: n }, (_, i) => snap(typeof over === "function" ? over(i) : over));

const ids = (h: ReturnType<typeof assessHealth>) => h.issues.map((i) => i.id);

describe("assessHealth", () => {
  test("a healthy stream is an A with no issues", () => {
    const h = assessHealth(run(8), visible);
    expect(h).toEqual({ grade: "A", score: 100, summary: "Smooth", issues: [] });
  });

  test("a hidden page is not graded", () => {
    const h = assessHealth(run(8, { fps: 2 }), { visible: false });
    expect(h.grade).toBeNull();
    expect(h.score).toBeNull();
    expect(h.summary).toContain("hidden");
  });

  test("too few snapshots are not graded", () => {
    expect(assessHealth([], visible).grade).toBeNull();
    expect(assessHealth(run(1), visible).grade).toBeNull();
    expect(assessHealth(run(2), visible).grade).toBe("A");
  });

  test("only the last window counts", () => {
    const old = run(20, { fps: 5 });
    expect(assessHealth([...old, ...run(HEALTH_WINDOW)], visible).grade).toBe("A");
  });

  describe("frame rate", () => {
    test("sustained half rate is a major stutter", () => {
      const h = assessHealth(run(8, { fps: 30 }), visible);
      expect(ids(h)).toContain("stutter");
      expect(h.issues.find((i) => i.id === "stutter")!.detail).toContain("30 fps shown of 60 sent");
      expect(["D", "F"]).toContain(h.grade!);
    });

    test("a mild shortfall is minor and costs an A", () => {
      const h = assessHealth(run(8, { fps: 56 }), visible);
      expect(h.issues.map((i) => i.severity)).toEqual(["minor"]);
      expect(h.grade).toBe("B");
    });

    test("a static desktop (almost nothing sent) is an A, not a stutter or freeze", () => {
      const h = assessHealth(run(8, { fps: 1, sentFps: 0.5, frameGapMs: 1000, mbps: 0.01 }), visible);
      expect(h).toMatchObject({ grade: "A", issues: [] });
      expect(assessHealth(run(8, { fps: 0, sentFps: 0, frameGapMs: 3000, deliveryMs: null, deliveryP95Ms: null }), visible).grade).toBe("A");
    });

    test("a slow source that shows what it sends is fine", () => {
      expect(assessHealth(run(8, { fps: 24, sentFps: 24, frameGapMs: 45 }), visible).grade).toBe("A");
    });

    test("an unknown send rate (older streamer) judges neither stutter nor freeze", () => {
      const h = assessHealth(run(8, { fps: 5, sentFps: null, frameGapMs: 900 }), visible);
      expect(h).toMatchObject({ grade: "A", issues: [] });
    });

    test("a burst then a still screen is smooth: shown is judged over the send rate's own span", () => {
      // The last second shows nothing, but over the reports' span every sent frame showed.
      const h = assessHealth(run(8, { fps: 0, sentFps: 12, shownSentFps: 12, frameGapMs: 4 }), visible);
      expect(h).toMatchObject({ grade: "A", issues: [] });
    });

    test("a busy source with far fewer frames shown is a stutter", () => {
      const h = assessHealth(run(8, { fps: 20, sentFps: 60 }), visible);
      expect(h.issues.find((i) => i.id === "stutter")!.detail).toContain("20 fps shown of 60 sent");
    });

    test("dropped frames, from the counter's growth", () => {
      const h = assessHealth(run(8, (i) => ({ framesDropped: 1000 + i * 6 })), visible);
      expect(ids(h)).toEqual(["dropped"]);
      expect(h.grade).toBe("C");
    });

    test("an old total of drops is not a current problem", () => {
      expect(assessHealth(run(8, { framesDropped: 5000 }), visible).grade).toBe("A");
    });

    test("a counter that resets (reconnect) counts as no growth", () => {
      const h = assessHealth(run(8, (i) => ({ framesDropped: i < 4 ? 900 + i : 2 })), visible);
      expect(ids(h)).toEqual([]);
    });
  });

  describe("decode", () => {
    test("decoding over the frame budget is critical and says what to try", () => {
      const h = assessHealth(run(8, { decodeMs: 20 }), visible);
      const issue = h.issues.find((i) => i.id === "decode")!;
      expect(issue.severity).toBe("critical");
      expect(issue.hint).toContain("60 fps");
      expect(["D", "F"]).toContain(h.grade!);
    });

    test("the budget scales with the frame rate", () => {
      // 10 ms is fine at 60 fps (budget 16.7) but over the budget at 120 (8.3).
      expect(assessHealth(run(8, { decodeMs: 9 }), visible).grade).toBe("B");
      expect(ids(assessHealth(run(8, { decodeMs: 6, targetFps: 120, sentFps: 120, fps: 120 }), visible))).toContain("decode");
      expect(assessHealth(run(8, { decodeMs: 3 }), visible).grade).toBe("A");
    });

    test("isn't judged on a still picture: a few slow keyframes hold nothing up", () => {
      const still = assessHealth(run(8, { decodeMs: 16, sentFps: 0.5, fps: 0 }), visible);
      expect(ids(still)).not.toContain("decode");
      expect(still.grade).toBe("A");
    });
  });

  describe("latency", () => {
    test("under 20 ms is clean", () => {
      expect(assessHealth(run(8, { latencyMs: 19 }), visible).grade).toBe("A");
    });

    test("bands: ~35 minor, ~60 major, 100+ critical", () => {
      const sev = (latencyMs: number) => assessHealth(run(8, { latencyMs }), visible).issues.find((i) => i.id === "latency")?.severity;
      expect(sev(25)).toBe("minor");
      expect(sev(35)).toBe("minor");
      expect(sev(60)).toBe("major");
      expect(sev(110)).toBe("critical");
      expect(assessHealth(run(8, { latencyMs: 110 }), visible).grade).toMatch(/[DF]/);
      expect(assessHealth(run(8, { latencyMs: 60 }), visible).grade).toBe("C");
    });

    test("a wide delivery spread is uneven latency", () => {
      const h = assessHealth(run(8, { deliveryMs: 6, deliveryP95Ms: 60 }), visible);
      expect(ids(h)).toContain("jitter");
    });

    test("latency wandering across the window is uneven latency", () => {
      const h = assessHealth(run(8, (i) => ({ latencyMs: i % 2 ? 40 : 10 })), visible);
      expect(ids(h)).toContain("jitter");
    });

    test("a WebRTC jitter buffer that grows", () => {
      const h = assessHealth(run(8, { deliveryMs: null, deliveryP95Ms: null, frameGapMs: null, jitterMs: 70 }), visible);
      expect(h.issues.find((i) => i.id === "jitter")!.detail).toContain("jitter buffer");
    });
  });

  describe("network", () => {
    test("lost frames over WebTransport", () => {
      const h = assessHealth(run(8, (i) => ({ packetsLost: i * 3 })), visible);
      expect(h.issues.find((i) => i.id === "loss")!.detail).toContain("frames lost");
      expect(h.grade).not.toBe("A");
      expect(h.issues.find((i) => i.id === "loss")!.hint).toContain("cable");
    });

    test("WebRTC packet counts are judged on a looser scale", () => {
      const rtc = { deliveryMs: null, deliveryP95Ms: null, frameGapMs: null };
      expect(assessHealth(run(8, (i) => ({ ...rtc, packetsLost: i * 1 })), visible).grade).toBe("A");
      const h = assessHealth(run(8, (i) => ({ ...rtc, packetsLost: i * 40 })), visible);
      expect(h.issues.find((i) => i.id === "loss")!.detail).toContain("packets lost");
    });

    test("FEC repairs are a minor note, not a failure", () => {
      const h = assessHealth(run(8, (i) => ({ framesRecovered: i * 8 })), visible);
      expect(ids(h)).toEqual(["recovered"]);
      expect(h.issues[0]!.severity).not.toBe("critical");
      expect(h.grade).toMatch(/[BC]/);
    });

    test("a long round trip", () => {
      const h = assessHealth(run(8, { rttMs: 50 }), visible);
      expect(ids(h)).toEqual(["rtt"]);
    });
  });

  describe("freezes", () => {
    test("a long gap between frames is critical, even once", () => {
      const h = assessHealth(run(8, (i) => ({ frameGapMs: i === 6 ? 600 : 20 })), visible);
      expect(ids(h)).toContain("freeze");
      expect(h.grade).not.toBe("A");
    });

    test("sustained freezing is an F", () => {
      const h = assessHealth(run(8, { frameGapMs: 800, fps: 20 }), visible);
      expect(h.grade).toBe("F");
      expect(h.issues[0]!.severity).toBe("critical");
    });

    test("frames sent but a 400 ms gap is a freeze", () => {
      const h = assessHealth(run(8, (i) => ({ frameGapMs: i === 6 ? 400 : 20 })), visible);
      expect(ids(h)).toContain("freeze");
    });

    test("the same gap while nothing was being sent is not", () => {
      expect(ids(assessHealth(run(8, (i) => ({ sentFps: 2, fps: 2, frameGapMs: i === 6 ? 400 : 20 })), visible))).not.toContain("freeze");
    });

    test("WebRTC with no frames shown counts as a freeze", () => {
      const h = assessHealth(run(8, { deliveryMs: null, deliveryP95Ms: null, frameGapMs: null, fps: 0, sentFps: 60 }), visible);
      expect(ids(h)).toContain("freeze");
    });

    test("a gap of a couple of frames is normal", () => {
      expect(assessHealth(run(8, { frameGapMs: 30 }), visible).grade).toBe("A");
    });
  });

  describe("node", () => {
    const node = (over: Partial<NodeStats>) => ({ node: { ...healthyNode, ...over } });

    test.each([
      ["CPU", { cpu: 97 }],
      ["GPU", { gpu: 99 }],
      ["VRAM", { vramUsed: 11.8 }],
      ["NVENC", { enc: 99 }],
      ["streamer CPU", { streamerCpu: 1500 }],
      ["RAM", { memUsed: 31.5 }],
    ])("%s saturated", (name, over) => {
      const h = assessHealth(run(8, node(over)), visible);
      expect(ids(h)).toEqual(["node"]);
      expect(h.issues[0]!.detail).toContain(name);
      expect(h.grade).not.toBe("A");
    });

    test("busy but under the limits is fine", () => {
      expect(assessHealth(run(8, node({ cpu: 80, gpu: 90, enc: 80 })), visible).grade).toBe("A");
    });

    test("no report (older streamer, or stale) is not penalised", () => {
      expect(assessHealth(run(8, { node: null }), visible).grade).toBe("A");
    });

    test("a CPU-only node has no GPU fields", () => {
      const h = assessHealth(run(8, { node: { ...healthyNode, gpu: undefined, vramUsed: undefined, vramTotal: undefined, enc: undefined, cpu: 96 } }), visible);
      expect(h.issues[0]!.detail).toBe("CPU 96%");
    });
  });

  describe("audio", () => {
    test("a growing audio buffer", () => {
      const h = assessHealth(run(8, { audioJitterMs: 150 }), visible);
      expect(ids(h)).toEqual(["audio"]);
    });
    test("a normal one", () => {
      expect(assessHealth(run(8, { audioJitterMs: 15 }), visible).grade).toBe("A");
    });
    test("NetEq's usual 30–60 ms on WebRTC is no issue, but well past it is", () => {
      const rtc = { deliveryMs: null, deliveryP95Ms: null, frameGapMs: null };
      expect(assessHealth(run(8, { ...rtc, audioJitterMs: 50 }), visible).grade).toBe("A");
      expect(ids(assessHealth(run(8, { ...rtc, audioJitterMs: 200 }), visible))).toEqual(["audio"]);
    });
    test("our WebTransport buffer growing past 20 ms is late packets", () => {
      expect(ids(assessHealth(run(8, { audioJitterMs: 50 }), visible))).toEqual(["audio"]);
    });
  });

  describe("spikes and sustained trouble", () => {
    test("one bad second doesn't flash an F", () => {
      const h = assessHealth(run(8, (i) => (i === 5 ? { fps: 10, frameGapMs: 400 } : {})), visible);
      expect(h.grade).not.toBe("F");
      expect(h.grade).not.toBe("A");
      expect(h.issues.every((i) => i.severity !== "critical")).toBe(true);
    });

    test("the same trouble sustained is worse than the spike", () => {
      const spike = assessHealth(run(8, (i) => (i === 5 ? { latencyMs: 120 } : {})), visible);
      const steady = assessHealth(run(8, { latencyMs: 120 }), visible);
      expect(steady.score!).toBeLessThan(spike.score!);
    });

    test("a spike that has aged out of the window is forgotten", () => {
      const h = assessHealth([...run(1, { latencyMs: 200 }), ...run(HEALTH_WINDOW)], visible);
      expect(h.grade).toBe("A");
    });
  });

  describe("combined", () => {
    test("several problems sum, most severe first", () => {
      const h = assessHealth(run(8, (i) => ({ fps: 35, decodeMs: 25, latencyMs: 90, packetsLost: i * 4, node: { ...healthyNode, cpu: 99 } })), visible);
      expect(h.grade).toBe("F");
      expect(h.score!).toBeLessThan(55);
      expect(h.issues.length).toBeGreaterThanOrEqual(4);
      const rank = { critical: 2, major: 1, minor: 0 };
      const ranks = h.issues.map((i) => rank[i.severity]);
      expect(ranks).toEqual([...ranks].sort((a, b) => b - a));
    });

    test("the summary names the worst issue", () => {
      expect(assessHealth(run(8, { decodeMs: 20 }), visible).summary).toBe("Slow decoding");
      expect(assessHealth(run(8, { node: { ...healthyNode, cpu: 99 } }), visible).summary).toBe("Node overloaded");
    });
  });

  describe("missing fields", () => {
    test("a WebRTC snapshot of a healthy stream is an A", () => {
      const rtc = { deliveryMs: null, deliveryP95Ms: null, frameGapMs: null, jitterMs: 4, audioJitterMs: 12, node: null };
      expect(assessHealth(run(8, rtc), visible)).toMatchObject({ grade: "A", issues: [] });
    });

    test("everything unknown is not penalised", () => {
      const bare = {
        fps: null,
        targetFps: null,
        sentFps: null,
        mbps: null,
        decodeMs: null,
        jitterMs: null,
        rttMs: null,
        latencyMs: null,
        deliveryMs: null,
        deliveryP95Ms: null,
        frameGapMs: null,
        audioJitterMs: null,
        node: null,
        codec: null,
        width: null,
        height: null,
      };
      expect(assessHealth(run(8, bare), visible)).toMatchObject({ grade: "A", score: 100 });
    });

    test("a snapshot with NaN fields doesn't crash or penalise", () => {
      expect(assessHealth(run(8, { decodeMs: NaN, latencyMs: NaN, rttMs: NaN }), visible).grade).toBe("A");
    });
  });

  describe("grade bands", () => {
    test("score maps to letters", () => {
      const grade = (decodeMs: number) => assessHealth(run(8, { decodeMs }), visible);
      expect(grade(3).grade).toBe("A");
      // Every grade is reachable through the scale of a single signal plus the caps.
      const seen = new Set([20, 12, 9, 7, 3].map((d) => grade(d).grade));
      expect(seen.size).toBeGreaterThanOrEqual(3);
    });
  });
});
