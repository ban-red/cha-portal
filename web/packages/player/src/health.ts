// A letter grade for the stream right now, from the last few seconds of stats snapshots.
//
// The numbers, weights and words are in `@cha/ui-spec`'s health.json, shared with Cha Player
// (crates/cha-player/src/health.rs); this file holds the checks, which read them. Shared cases
// in health-cases.json keep the two judging alike (see web/packages/ui-spec/README.md).

import { type Band, fill, HEALTH, healthIssues, textFor } from "@cha/ui-spec";

import type { NodeStats, StatsSnapshot } from "./stats";

export type HealthGrade = "A" | "B" | "C" | "D" | "F";
export type IssueSeverity = "minor" | "major" | "critical";

export interface HealthIssue {
  id: string;
  severity: IssueSeverity;
  /** A few words: what is wrong. */
  title: string;
  /** What was measured, with the numbers. */
  detail: string;
  /** What it means and what to try. */
  hint: string;
}

export interface HealthAssessment {
  /** Null while there is nothing to judge: the page is hidden, or fewer than `MIN_SNAPSHOTS` snapshots came in. */
  grade: HealthGrade | null;
  /** 0..100, null with the grade. */
  score: number | null;
  /** One line for the grade: "Smooth", "Some stutter", … */
  summary: string;
  /** Most severe first. Empty for an A. */
  issues: HealthIssue[];
}

export interface HealthContext {
  /** Whether the page is visible. A hidden page throttles frames and timers, so its stats say nothing about the stream. */
  visible: boolean;
  /** Time between snapshots, for turning counter deltas into rates (default 1000, what the portal polls at). */
  intervalMs?: number;
}

/** Snapshots judged: the last few seconds. */
export const HEALTH_WINDOW = HEALTH.window;
/** Fewer snapshots than this and the counters' deltas and the spread mean nothing. */
export const MIN_SNAPSHOTS = HEALTH.min_snapshots;

const PLATFORM = "web";
const LEVELS = HEALTH.levels;
const P = HEALTH.params;
const B = HEALTH.bands;

/**
 * A snapshot's level for a signal is 0 below the band's `from`, then `base` at `from` rising
 * linearly to base + slope at `to`.
 */
function level(v: number | null | undefined, band: Band): number {
  if (v === null || v === undefined || !Number.isFinite(v) || v < band.from) return 0;
  return LEVELS.base + LEVELS.slope * Math.min(1, (v - band.from) / (band.to - band.from));
}

/** The same for a signal where lower is worse (`from` above `to`). */
function levelBelow(v: number | null | undefined, band: Band): number {
  if (v === null || v === undefined || !Number.isFinite(v) || v > band.from) return 0;
  return LEVELS.base + LEVELS.slope * Math.min(1, (band.from - v) / (band.from - band.to));
}

/** What a check found: the window's level, and the text for it. */
interface Finding {
  level: number;
  detail: string;
}

interface Ctx {
  /** The streamer's frame rate, or the fallback until it says (for the frame budget and drop rate). */
  target: number;
  budgetMs: number;
  seconds: number;
}

const mean = (a: number[]) => a.reduce((x, y) => x + y, 0) / a.length;

/** The detail text of an issue's variant on this platform, filled in. */
function detail(id: string, variant: string, values: Record<string, number | string>): string {
  const issue = HEALTH.issues.find((i) => i.id === id)!;
  return fill(textFor(issue.details[variant]!, PLATFORM, `${id} detail ${variant}`), values);
}

/** Mean and worst of per-snapshot levels: sustained trouble counts, and so does a spike, but a single one stays minor. */
function windowLevel(levels: number[]): number {
  if (!levels.length) return 0;
  return Math.max(mean(levels), LEVELS.spike_share * Math.max(...levels));
}

/** The snapshots' values for a field, where it is present. */
function values<T>(window: StatsSnapshot[], pick: (s: StatsSnapshot) => T | null | undefined): T[] {
  const out: T[] = [];
  for (const s of window) {
    const v = pick(s);
    if (v !== null && v !== undefined) out.push(v);
  }
  return out;
}

/** The growth of a cumulative counter across the window; a drop (a reconnect reset it) counts as zero. */
function growth(window: StatsSnapshot[], pick: (s: StatsSnapshot) => number): number {
  let total = 0;
  for (let i = 1; i < window.length; i++) total += Math.max(0, pick(window[i]!) - pick(window[i - 1]!));
  return total;
}

/** The spread of the middle half: sustained wandering shows, one outlier doesn't (that is the latency check's job). */
function interquartile(a: number[]): number {
  const s = [...a].sort((x, y) => x - y);
  const at = (q: number) => s[Math.round(q * (s.length - 1))]!;
  return at(0.75) - at(0.25);
}

/** A level for a window-wide rate, with the rate's text. */
function rateFinding(rate: number, band: Band, text: (rate: number) => string): Finding | null {
  const l = level(rate, band);
  return l > 0 ? { level: l, detail: text(rate) } : null;
}

/** Frames per second shown over the send rate's span, or the last second's on a snapshot without it. */
const shownOfSent = (s: StatsSnapshot) => s.shownSentFps ?? s.fps;

/** Whether the snapshot is of a source sending at a rate, so stutter and freezes can be judged. */
const isBusy = (s: StatsSnapshot) => s.sentFps !== null && s.sentFps >= P.busy_sent_fps;

/** Whether the stream is WebTransport, which has delivery times and frame gaps that WebRTC lacks. */
const isWebTransport = (window: StatsSnapshot[]) => window.some((s) => s.deliveryMs !== null || s.frameGapMs !== null);
/** PyroWave: every frame stands alone, so a lost one is skipped, not a broken chain. */
const isPyroWave = (window: StatsSnapshot[]) => window.some((s) => s.codec?.startsWith("PYROWAVE") ?? false);

/** The checks, by issue id. Titles, hints, weights and the words of a detail are the spec's. */
const CHECKS: Record<string, (window: StatsSnapshot[], ctx: Ctx) => Finding | null> = {
  stutter(window) {
    // Shown over the same span as sent: the last second's `fps` against a few seconds' send
    // rate would call a burst followed by a still screen a stutter.
    const busy = window.filter((s) => isBusy(s) && shownOfSent(s) !== null);
    const l = windowLevel(busy.map((s) => levelBelow(shownOfSent(s)! / s.sentFps!, B.fps_ratio)));
    if (l === 0) return null;
    return {
      level: l,
      detail: detail("stutter", "main", {
        shown: mean(busy.map((s) => shownOfSent(s)!)),
        sent: mean(busy.map((s) => s.sentFps!)),
      }),
    };
  },

  dropped(window, { target, seconds }) {
    if (seconds <= 0) return null;
    const rate = growth(window, (s) => s.framesDropped) / seconds;
    return rateFinding(rate / target, B.dropped, () => detail("dropped", "main", { rate }));
  },

  decode(window, { budgetMs }) {
    // Only while frames come at a rate: a few frames on a still picture
    // (often keyframes, the slowest to decode) hold nothing up.
    const v = values(window.filter(isBusy), (s) => s.decodeMs);
    const l = windowLevel(v.map((d) => level(d / budgetMs, B.decode_budget)));
    if (l === 0) return null;
    return { level: l, detail: detail("decode", "main", { decode: mean(v), budget: budgetMs }) };
  },

  freeze(window, { budgetMs }) {
    const levels: number[] = [];
    let worst = 0;
    for (const s of window) {
      // Only while frames were being sent: a still picture has long gaps by nature. A source
      // sending slowly has a long time between frames, so the gap must also beat that many send intervals.
      if (!isBusy(s)) continue;
      // A WebRTC page has no gap measure, but a second with no frames at all is one.
      const gap = s.frameGapMs !== null ? s.frameGapMs : shownOfSent(s) === 0 ? P.no_frames_gap_ms : null;
      if (gap === null) continue;
      const from = Math.max(P.freeze_min_ms, P.freeze_budgets * budgetMs, (P.freeze_budgets * 1000) / s.sentFps!);
      levels.push(level(gap, { from, to: P.freeze_to_ms }));
      worst = Math.max(worst, gap);
    }
    const l = windowLevel(levels);
    if (l === 0) return null;
    return { level: l, detail: detail("freeze", "main", { gap: worst }) };
  },

  latency(window) {
    const v = values(window, (s) => s.latencyMs);
    const l = windowLevel(v.map((d) => level(d, B.latency)));
    if (l === 0) return null;
    return { level: l, detail: detail("latency", "main", { latency: mean(v) }) };
  },

  jitter(window) {
    const candidates: Finding[] = [];
    const spreads = values(window, (s) => (s.deliveryMs !== null && s.deliveryP95Ms !== null ? s.deliveryP95Ms - s.deliveryMs : null));
    if (spreads.length) {
      const l = windowLevel(spreads.map((d) => level(d, B.delivery_spread)));
      if (l > 0) candidates.push({ level: l, detail: detail("jitter", "delivery", { spread: Math.max(...spreads) }) });
    }
    const lat = values(window, (s) => s.latencyMs);
    if (lat.length >= P.latency_spread_samples) {
      const sd = interquartile(lat);
      const l = level(sd, B.latency_spread);
      if (l > 0) candidates.push({ level: l, detail: detail("jitter", "latency", { spread: sd }) });
    }
    const buf = values(window, (s) => s.jitterMs);
    if (buf.length) {
      const l = windowLevel(buf.map((d) => level(d, B.jitter_buffer)));
      if (l > 0) candidates.push({ level: l, detail: detail("jitter", "buffer", { wait: mean(buf) }) });
    }
    return candidates.sort((a, b) => b.level - a.level)[0] ?? null;
  },

  loss(window, { seconds }) {
    if (seconds <= 0 || isPyroWave(window)) return null;
    const wt = isWebTransport(window);
    const rate = growth(window, (s) => s.packetsLost) / seconds;
    return rateFinding(rate, wt ? B.lost_frames : B.lost_packets, () => detail("loss", "main", { rate, unit: wt ? "frames" : "packets" }));
  },

  recovered(window, { seconds }) {
    if (seconds <= 0) return null;
    const rate = growth(window, (s) => s.framesRecovered) / seconds;
    return rateFinding(rate, B.recovered, () => detail("recovered", "main", { rate }));
  },

  skipped(window, { seconds, target }) {
    if (seconds <= 0 || !isPyroWave(window)) return null;
    const rate = growth(window, (s) => s.packetsLost) / seconds;
    return rateFinding(rate / target, B.skipped, () => detail("skipped", "main", { rate }));
  },

  partial(window, { seconds }) {
    if (seconds <= 0) return null;
    const rate = growth(window, (s) => s.framesPartial ?? 0) / seconds;
    return rateFinding(rate, B.partial, () => detail("partial", "main", { rate }));
  },

  rtt(window) {
    const v = values(window, (s) => s.rttMs);
    const l = windowLevel(v.map((d) => level(d, B.rtt)));
    if (l === 0) return null;
    return { level: l, detail: detail("rtt", "main", { rtt: mean(v) }) };
  },

  node(window, { budgetMs }) {
    const per = window
      .filter((s) => s.node)
      .map((s) => nodeLevels(s.node!, s.encodeP99Ms != null && s.encodeP99Ms > P.gpu_late_encode * budgetMs));
    if (!per.length) return null;
    const l = windowLevel(per.map((p) => Math.max(0, ...p.map((x) => x.level))));
    if (l === 0) return null;
    // Name whatever was over in the worst reading.
    const worst = per.reduce((a, b) => (Math.max(0, ...b.map((x) => x.level)) > Math.max(0, ...a.map((x) => x.level)) ? b : a));
    const parts = worst.filter((x) => x.level > 0).map((x) => x.text);
    return { level: l, detail: detail("node", "main", { parts: parts.join(HEALTH.text.list_separator) }) };
  },

  "sound-restart"(window) {
    const n = growth(window, (s) => s.audioRestarts ?? 0);
    if (n <= 0) return null;
    // One in the window is a note; several is real trouble.
    return {
      level: Math.min(1, P.sound_restart_first + P.sound_restart_step * (n - 1)),
      detail: detail("sound-restart", "main", { count: n, s: n === 1 ? "" : "s" }),
    };
  },

  "sound-out"(window) {
    // "Sound in it": the decoded sound handed to the output had a sample above `sound_present` in
    // that second, while the output played a peak under `sound_played` (the stats are null while the
    // output waits for a click, which isn't this). Several seconds of it, not a blip.
    const dead = window.filter(
      (s) => s.audioInPeak != null && s.audioOutPeak != null && s.audioInPeak > P.sound_present && s.audioOutPeak < P.sound_played,
    );
    if (dead.length < P.sound_out_seconds) return null;
    return {
      level: Math.min(1, P.sound_out_first + P.sound_out_step * (dead.length - P.sound_out_seconds)),
      detail: detail("sound-out", "main", { peak: mean(dead.map((s) => s.audioInPeak!)), seconds: dead.length }),
    };
  },

  audio(window) {
    const v = values(window, (s) => s.audioJitterMs);
    const band = isWebTransport(window) ? B.audio_jitter_wt : B.audio_jitter_rtc;
    const l = windowLevel(v.map((d) => level(d, band)));
    if (l === 0) return null;
    return { level: l, detail: detail("audio", "main", { buffer: mean(v) }) };
  },
};

/** The ids of the checks implemented here, for the coverage test. */
export const CHECK_IDS = Object.keys(CHECKS);

/** Each resource's level and phrase; the GPU's only while `encodeLate` (see `gpu_late_encode`). */
function nodeLevels(n: NodeStats, encodeLate: boolean): { level: number; text: string }[] {
  const text = (variant: string, pct: number) => detail("node", variant, { pct });
  return [
    { level: level(n.cpu, B.node_cpu), text: text("cpu", n.cpu) },
    { level: n.memTotal > 0 ? level((n.memUsed / n.memTotal) * 100, B.node_ram) : 0, text: text("ram", (n.memUsed / n.memTotal) * 100) },
    { level: encodeLate ? level(n.gpu, B.node_gpu) : 0, text: text("gpu", n.gpu ?? 0) },
    {
      level: n.vramTotal && n.vramUsed !== undefined ? level((n.vramUsed / n.vramTotal) * 100, B.node_vram) : 0,
      text: text("vram", ((n.vramUsed ?? 0) / (n.vramTotal || 1)) * 100),
    },
    { level: level(n.enc, B.node_enc), text: text("enc", n.enc ?? 0) },
    { level: level(n.cores > 0 ? n.streamerCpu / n.cores : n.streamerCpu, B.streamer_cpu), text: text("streamer", n.streamerCpu) },
  ];
}

const severityOf = (l: number): IssueSeverity => (l >= LEVELS.critical ? "critical" : l >= LEVELS.major ? "major" : "minor");
const SEVERITY_RANK: Record<IssueSeverity, number> = { critical: 2, major: 1, minor: 0 };

/**
 * Grades the stream from `history` (oldest first; only the last `HEALTH_WINDOW` count).
 * Fields a snapshot lacks (an older streamer, or the other transport) are skipped, never penalised.
 */
export function assessHealth(history: StatsSnapshot[], context: HealthContext): HealthAssessment {
  if (!context.visible) return { grade: null, score: null, summary: HEALTH.text.hidden, issues: [] };
  const window = history.slice(-HEALTH_WINDOW);
  if (window.length < MIN_SNAPSHOTS) return { grade: null, score: null, summary: HEALTH.text.measuring, issues: [] };

  const latest = window[window.length - 1]!;
  const target = latest.targetFps && latest.targetFps > 0 ? latest.targetFps : HEALTH.fallback_fps;
  const ctx: Ctx = {
    target,
    budgetMs: 1000 / target,
    seconds: ((window.length - 1) * (context.intervalMs ?? 1000)) / 1000,
  };

  const found: { issue: HealthIssue; penalty: number; summary: string }[] = [];
  for (const spec of healthIssues(PLATFORM)) {
    const f = CHECKS[spec.id]?.(window, ctx);
    if (!f || f.level < LEVELS.min) continue;
    found.push({
      issue: {
        id: spec.id,
        severity: severityOf(f.level),
        title: textFor(spec.title, PLATFORM, `${spec.id} title`),
        detail: f.detail,
        hint: textFor(spec.hint, PLATFORM, `${spec.id} hint`),
      },
      penalty: spec.weight * f.level,
      summary: spec.summary,
    });
  }
  found.sort((a, b) => SEVERITY_RANK[b.issue.severity] - SEVERITY_RANK[a.issue.severity] || b.penalty - a.penalty);

  let score = 100 - found.reduce((sum, f) => sum + f.penalty, 0);
  for (const f of found) score = Math.min(score, HEALTH.severity_cap[f.issue.severity]);
  score = Math.max(0, Math.round(score));
  const grade = HEALTH.grades.find(({ floor }) => score >= floor)!.grade;
  return {
    grade,
    score,
    summary: found.length ? found[0]!.summary : HEALTH.text.smooth,
    issues: found.map((f) => f.issue),
  };
}
