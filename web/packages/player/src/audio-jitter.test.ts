import { describe, expect, test } from "bun:test";

import { AudioJitter, DECAY_MS_PER_S, MAX_DELAY_MS, type AudioOut } from "./audio-jitter";

interface Result {
  jitter: AudioJitter<number>;
  played: number[];
  out: AudioOut<number>[];
  /** The delay at each simulated second. */
  delays: number[];
  /** Glitches (concealed, dropped, late) so far at each simulated second. */
  glitchesAt: number[];
}

/**
 * Sends `count` packets 10 ms apart; `network(i)` is each one's transit in ms (null: lost)
 * and `extra(i)` an arrival time added on top, for reordering. The receiver ticks every ms.
 */
function simulate(
  count: number,
  network: (i: number) => number | null,
  opts: { dup?: (i: number) => boolean; tailMs?: number } = {},
): Result {
  const jitter = new AudioJitter<number>();
  const arrivals: { at: number; id: number }[] = [];
  const offsetUs = 4_294_000_000; // the server's clock is near its 32-bit wrap
  for (let i = 0; i < count; i++) {
    const transit = network(i);
    if (transit === null) continue;
    arrivals.push({ at: i * 10 + transit, id: i });
    if (opts.dup?.(i)) arrivals.push({ at: i * 10 + transit + 3, id: i });
  }
  arrivals.sort((a, b) => a.at - b.at);
  const out: AudioOut<number>[] = [];
  const delays: number[] = [];
  const glitchesAt: number[] = [];
  let next = 0;
  const end = count * 10 + (opts.tailMs ?? 200);
  for (let t = 0; t <= end; t++) {
    while (next < arrivals.length && arrivals[next]!.at <= t) {
      const a = arrivals[next++]!;
      jitter.push(a.id, (a.id * 10_000 + offsetUs) >>> 0, t, a.id);
    }
    out.push(...jitter.poll(t));
    if (t % 1000 === 0) {
      delays.push(jitter.delayMs);
      const s = jitter.stats;
      glitchesAt.push(s.concealed + s.dropped + s.late);
    }
  }
  const played = out.flatMap((o) => (o.kind === "play" ? [o.id] : []));
  return { jitter, played, out, delays, glitchesAt };
}

const glitches = (r: Result) => r.jitter.stats.concealed + r.jitter.stats.dropped + r.jitter.stats.late;
/** A deterministic pseudo-random in [0, 1). */
const rand = (seed: number) => {
  let s = seed;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
};

describe("AudioJitter", () => {
  test("an in-order LAN stream plays on arrival and its delay stays about 0", () => {
    const r = simulate(1000, () => 3);
    expect(r.played).toEqual(Array.from({ length: 1000 }, (_, i) => i));
    expect(glitches(r)).toBe(0);
    expect(Math.max(...r.delays)).toBeLessThan(2);
  });

  test("a LAN stream with sub-millisecond jitter keeps the delay at 0", () => {
    const rnd = rand(1);
    const r = simulate(1000, () => 3 + rnd() * 0.8);
    expect(r.jitter.delayMs).toBe(0);
    expect(glitches(r)).toBe(0);
  });

  test("packets play the moment they arrive when there is no delay", () => {
    const j = new AudioJitter<number>();
    expect(j.poll(0)).toEqual([]);
    j.push(0, 0, 100, 0);
    expect(j.poll(100)).toEqual([{ kind: "play", id: 0, data: 0 }]);
    j.push(1, 10_000, 110, 1);
    expect(j.poll(110)).toEqual([{ kind: "play", id: 1, data: 1 }]);
  });

  test("a jittered stream grows the delay to cover it, and the glitches stop", () => {
    const rnd = rand(7);
    const r = simulate(3000, () => 5 + rnd() * 40);
    // After it has learned (the first seconds), the delay covers the ~40 ms spread.
    expect(r.jitter.delayMs).toBeGreaterThan(30);
    expect(r.jitter.delayMs).toBeLessThanOrEqual(MAX_DELAY_MS);
    // It learns in the first seconds; after that under 1% of the packets (the
    // tail the p99 leaves) is concealed or dropped.
    expect(r.glitchesAt[r.glitchesAt.length - 1]! - r.glitchesAt[10]!).toBeLessThan(0.01 * 1900);
    expect(r.played.length + glitches(r)).toBeGreaterThanOrEqual(3000 - 5);
  });

  test("packets are handed out in id order when they arrive reordered", () => {
    // Every 5th packet takes 12 ms longer, so it arrives behind its successor.
    const r = simulate(500, (i) => (i % 5 === 0 ? 20 : 5));
    const sorted = [...r.played].sort((a, b) => a - b);
    expect(r.played).toEqual(sorted);
    expect(new Set(r.played).size).toBe(r.played.length);
    // The delay grew to cover the reordering, so almost all of them played.
    expect(r.played.length).toBeGreaterThan(480);
  });

  test("duplicates play once", () => {
    const r = simulate(300, () => 4, { dup: (i) => i % 3 === 0 });
    expect(r.played).toEqual(Array.from({ length: 300 }, (_, i) => i));
    expect(r.jitter.stats.late).toBe(100);
  });

  test("a packet that arrives after its slot was skipped is dropped", () => {
    const j = new AudioJitter<number>();
    j.push(0, 0, 0, 0);
    j.push(2, 20_000, 20, 2);
    // Packet 1 is missing at its slot (10 ms): silence for it, then packet 2.
    expect(j.poll(0)).toEqual([{ kind: "play", id: 0, data: 0 }]);
    expect(j.poll(20)).toEqual([
      { kind: "conceal", id: 1 },
      { kind: "play", id: 2, data: 2 },
    ]);
    j.push(1, 10_000, 25, 1);
    expect(j.poll(25)).toEqual([]);
    expect(j.stats.late).toBe(1);
  });

  test("lost packets are concealed with 10 ms each and the rest play", () => {
    const r = simulate(1000, (i) => (i % 50 === 25 ? null : 4));
    expect(r.jitter.stats.concealed).toBe(20);
    expect(r.played.length).toBe(980);
    const concealed = r.out.flatMap((o) => (o.kind === "conceal" ? [o.id] : []));
    expect(concealed.every((id) => id % 50 === 25)).toBe(true);
  });

  test("the 32-bit send time wrapping mid-stream changes nothing", () => {
    // simulate() starts 0.29 s before the wrap, so 1000 packets cross it.
    const r = simulate(1000, () => 4);
    expect(glitches(r)).toBe(0);
    expect(r.jitter.delayMs).toBe(0);
  });

  test("a burst raises the delay and it comes back down slowly", () => {
    // 20 s quiet, 1 s of 40 ms jitter, then quiet again.
    const rnd = rand(3);
    const r = simulate(
      6000,
      (i) => (i >= 2000 && i < 2100 ? 5 + rnd() * 40 : 5),
      { tailMs: 0 },
    );
    const before = r.delays[19]!;
    const peak = Math.max(...r.delays.slice(20, 30));
    const after = r.delays[r.delays.length - 1]!;
    expect(before).toBeLessThan(2);
    expect(peak).toBeGreaterThan(25);
    // The window forgets the burst after 5 s, then the delay falls by DECAY_MS_PER_S, not at once.
    const i6 = 20 + 1 + 5 + 1; // 6 s after the burst, only a few seconds of decay
    expect(r.delays[i6]!).toBeGreaterThan(peak - DECAY_MS_PER_S * 4);
    expect(after).toBeLessThan(2);
  });

  test("a stall is skipped, not filled with seconds of silence", () => {
    const j = new AudioJitter<number>();
    j.push(0, 0, 0, 0);
    j.poll(0);
    // 2 s with nothing, then ids resume 200 ahead.
    j.push(200, 2_000_000, 2000, 200);
    const out = j.poll(2000);
    expect(out.filter((o) => o.kind === "conceal").length).toBe(0);
    expect(out).toEqual([{ kind: "play", id: 200, data: 200 }]);
    expect(j.stats.skipped).toBeGreaterThan(100);
  });

  test("a burst of packets long past their slots is dropped to catch up", () => {
    const j = new AudioJitter<number>();
    j.push(0, 0, 0, 0);
    j.poll(0);
    // A 300 ms freeze, then 30 packets at once: the delay jumps, and the stale ones go.
    for (let i = 1; i <= 30; i++) j.push(i, i * 10_000, 300, i);
    const played: number[] = [];
    for (let t = 300; t <= 500; t++) for (const o of j.poll(t)) if (o.kind === "play") played.push(o.id);
    expect(j.stats.dropped).toBeGreaterThan(0);
    expect(played.length + j.stats.dropped).toBe(30);
    expect(played).toEqual([...played].sort((a, b) => a - b));
    expect(played[played.length - 1]).toBe(30);
  });

  test("the next-due time tells the caller when to poll", () => {
    const j = new AudioJitter<number>();
    expect(j.nextDueMs(0)).toBeNull();
    j.push(0, 0, 0, 0);
    j.poll(0);
    j.push(2, 20_000, 18, 2); // 1 is missing
    const due = j.nextDueMs(18)!;
    expect(due).toBeGreaterThanOrEqual(0);
    expect(due).toBeLessThanOrEqual(2);
  });
});
