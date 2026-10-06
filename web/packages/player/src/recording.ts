// The summary of a measurement run: stats snapshots collected for a while (the portal's
// "Record 30 s"), reduced to the numbers the Phase 2 exit pass hands back. Pure.

import type { HealthAssessment, HealthGrade } from "./health";
import type { StatsSnapshot } from "./stats";

export interface RecordingMeta {
  userAgent: string;
  /** "webtransport" or "webrtc". */
  transport: string | null;
  /** The environment's id and the app's name. */
  environment: string | null;
  app: string | null;
  /** When the recording started (epoch ms). */
  startedAt: number;
  /** The wall-clock length asked for, s. */
  durationS: number;
  /** Time between snapshots, ms (default 1000). */
  intervalMs?: number;
  /** Every send → shown time of the run, ms (`Player.collectLatencies`); the per-second p50s are used if absent or empty. */
  latencySamples?: number[];
}

/** p50, p95, p99 of something, or all null. */
export interface Spread {
  p50: number | null;
  p95: number | null;
  p99: number | null;
}

export interface RecordingSummary {
  environment: string | null;
  app: string | null;
  /** ISO 8601. */
  timestamp: string;
  userAgent: string;
  transport: string | null;
  codec: string | null;
  /** The decoded picture size at the end. */
  width: number | null;
  height: number | null;
  /** Seconds covered, and snapshots taken (fewer than the seconds if the tab was hidden). */
  durationS: number;
  snapshots: number;
  fps: {
    /** The frame rate the streamer encodes at. */
    target: number | null;
    /** The shown rate per snapshot. */
    shownP50: number | null;
    shownMin: number | null;
    /** The mean frames per second the streamer sent (null on an older one): a still desktop sends few. */
    sentMean: number | null;
  };
  /** Send → shown, ms. `source` says whether the percentiles are over every frame or over the seconds' p50s. */
  latencyMs: Spread & { samples: number; source: "frames" | "per-second p50" };
  /** Per-second mean decode time, ms. */
  decodeMs: { p50: number | null; p95: number | null };
  /** Send → decoded p95 per second (WebTransport), ms. */
  deliveryP95Ms: number | null;
  rttMsP50: number | null;
  bitrateMbpsAvg: number | null;
  /** Counter growth over the run. */
  framesLost: number;
  framesRecovered: number;
  framesPartial: number;
  framesDropped: number;
  /** The longest wait between frames (WebTransport), ms, and the seconds in which a wait passed FREEZE_MS. */
  freezes: { longestGapMs: number | null; count: number };
  /** Sound's buffer delay: mean and largest, ms. */
  audioBufferMs: { mean: number | null; max: number | null };
  health: {
    /** The worst grade seen during the run and the grade at its end. */
    worstGrade: HealthGrade | null;
    finalGrade: HealthGrade | null;
    summary: string;
    /** The issues of the worst moment: "title: detail". */
    reasons: string[];
  };
}

/** The streamer sent at least this many frames a second: the picture was moving. */
const BUSY_SENT_FPS = 10;

/** A gap between frames this long is a freeze. */
export const FREEZE_MS = 250;
const GRADE_ORDER: HealthGrade[] = ["A", "B", "C", "D", "F"];

function pct(sorted: number[], p: number): number | null {
  if (!sorted.length) return null;
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))]!;
}
const sortedOf = (xs: (number | null | undefined)[]) =>
  xs.filter((x): x is number => typeof x === "number" && Number.isFinite(x)).sort((a, b) => a - b);
const mean = (xs: number[]) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : null);
const round = (v: number | null, digits = 2) => (v === null ? null : Number(v.toFixed(digits)));
/** Growth of a counter over the run; a reset (reconnect) counts what came after it. */
function growth(xs: number[]): number {
  let total = 0;
  for (let i = 1; i < xs.length; i++) total += xs[i]! >= xs[i - 1]! ? xs[i]! - xs[i - 1]! : xs[i]!;
  return total;
}

/**
 * Reduces a run's snapshots (oldest first) and, per snapshot, the health assessment at that
 * time. Fields a snapshot lacks (the other transport, an older streamer) are left out of the
 * numbers that need them, never counted as 0.
 */
export function summarizeRecording(
  snapshots: StatsSnapshot[],
  health: HealthAssessment[],
  meta: RecordingMeta,
): RecordingSummary {
  const last = snapshots[snapshots.length - 1];
  const lastWith = <K extends keyof StatsSnapshot>(k: K) => [...snapshots].reverse().find((s) => s[k] !== null)?.[k] ?? null;

  // A still screen sends nothing, and that isn't a freeze: shown fps and gaps
  // count only the seconds the streamer was sending (as the health grade does).
  const busy = snapshots.filter((s) => s.sentFps !== null && s.sentFps >= BUSY_SENT_FPS);
  const fps = sortedOf(busy.map((s) => s.shownSentFps ?? s.fps));
  const decode = sortedOf(snapshots.map((s) => s.decodeMs));
  const frames = meta.latencySamples?.length ? sortedOf(meta.latencySamples) : null;
  const perSecond = sortedOf(snapshots.map((s) => s.latencyMs));
  const lat = frames ?? perSecond;
  const gaps = busy.map((s) => s.frameGapMs).filter((g): g is number => g !== null);
  const audio = sortedOf(snapshots.map((s) => s.audioJitterMs));

  const graded = health.filter((h): h is HealthAssessment & { grade: HealthGrade; score: number } => h.grade !== null);
  const worst = graded.reduce<(typeof graded)[number] | null>((w, h) => (!w || h.score < w.score ? h : w), null);
  const finalH = graded[graded.length - 1] ?? null;
  const worstGrade = graded.length
    ? graded.map((h) => h.grade).sort((a, b) => GRADE_ORDER.indexOf(b) - GRADE_ORDER.indexOf(a))[0]!
    : null;

  return {
    environment: meta.environment,
    app: meta.app,
    timestamp: new Date(meta.startedAt).toISOString(),
    userAgent: meta.userAgent,
    transport: meta.transport,
    codec: lastWith("codec"),
    width: last?.width ?? null,
    height: last?.height ?? null,
    durationS: meta.durationS,
    snapshots: snapshots.length,
    fps: {
      target: lastWith("targetFps"),
      shownP50: round(pct(fps, 0.5), 1),
      shownMin: round(fps[0] ?? null, 1),
      sentMean: round(mean(sortedOf(snapshots.map((s) => s.sentFps))), 1),
    },
    latencyMs: {
      p50: round(pct(lat, 0.5)),
      p95: round(pct(lat, 0.95)),
      p99: round(pct(lat, 0.99)),
      samples: lat.length,
      source: frames ? "frames" : "per-second p50",
    },
    decodeMs: { p50: round(pct(decode, 0.5)), p95: round(pct(decode, 0.95)) },
    deliveryP95Ms: round(pct(sortedOf(snapshots.map((s) => s.deliveryP95Ms)), 0.5)),
    rttMsP50: round(pct(sortedOf(snapshots.map((s) => s.rttMs)), 0.5)),
    bitrateMbpsAvg: round(mean(sortedOf(snapshots.map((s) => s.mbps))), 1),
    framesLost: growth(snapshots.map((s) => s.packetsLost)),
    framesRecovered: growth(snapshots.map((s) => s.framesRecovered)),
    framesPartial: growth(snapshots.map((s) => s.framesPartial ?? 0)),
    framesDropped: growth(snapshots.map((s) => s.framesDropped)),
    freezes: {
      longestGapMs: gaps.length ? Math.round(Math.max(...gaps)) : null,
      count: gaps.filter((g) => g >= FREEZE_MS).length,
    },
    audioBufferMs: { mean: round(mean(audio), 1), max: round(audio[audio.length - 1] ?? null, 1) },
    health: {
      worstGrade,
      finalGrade: finalH?.grade ?? null,
      summary: worst?.summary ?? "Not graded",
      reasons: worst ? worst.issues.map((i) => `${i.title}: ${i.detail}`) : [],
    },
  };
}

const n = (v: number | null, unit = "") => (v === null ? "n/a" : `${v}${unit}`);

/** The summary as a Markdown table, to paste into an issue or the benchmark notes. */
export function recordingMarkdown(r: RecordingSummary): string {
  const rows: [string, string][] = [
    ["Environment", [r.app, r.environment].filter(Boolean).join(" · ") || "n/a"],
    ["When", r.timestamp],
    ["Browser", r.userAgent],
    ["Transport", r.transport ?? "n/a"],
    ["Codec", r.codec ?? "n/a"],
    ["Decoded size", r.width && r.height ? `${r.width}×${r.height}` : "n/a"],
    ["Run", `${r.durationS} s, ${r.snapshots} snapshots`],
    ["FPS shown vs target", `p50 ${n(r.fps.shownP50)}, min ${n(r.fps.shownMin)} of ${n(r.fps.target)} (streamer sent ${n(r.fps.sentMean)}/s on average)`],
    [
      `Send → shown (${r.latencyMs.samples} ${r.latencyMs.source === "frames" ? "frames" : "per-second p50s"})`,
      `p50 ${n(r.latencyMs.p50, " ms")}, p95 ${n(r.latencyMs.p95, " ms")}, p99 ${n(r.latencyMs.p99, " ms")}`,
    ],
    ["Decode (per-second mean)", `p50 ${n(r.decodeMs.p50, " ms")}, p95 ${n(r.decodeMs.p95, " ms")}`],
    ["Send → decoded p95", n(r.deliveryP95Ms, " ms")],
    ["RTT p50", n(r.rttMsP50, " ms")],
    ["Bitrate (mean)", n(r.bitrateMbpsAvg, " Mbit/s")],
    ["Lost / recovered / dropped frames", `${r.framesLost} / ${r.framesRecovered} / ${r.framesDropped}`],
    ["Frames shown incomplete (PyroWave)", `${r.framesPartial}`],
    ["Freezes", `${r.freezes.count} (≥ ${FREEZE_MS} ms); longest gap ${n(r.freezes.longestGapMs, " ms")}`],
    ["Audio buffer", `mean ${n(r.audioBufferMs.mean, " ms")}, max ${n(r.audioBufferMs.max, " ms")}`],
    ["Health", `worst ${r.health.worstGrade ?? "n/a"}, final ${r.health.finalGrade ?? "n/a"} (${r.health.summary})`],
  ];
  const lines = ["| | |", "|---|---|", ...rows.map(([k, v]) => `| ${k} | ${v.replace(/\|/g, "\\|")} |`)];
  if (r.health.reasons.length) lines.push("", "Health reasons:", ...r.health.reasons.map((x) => `- ${x}`));
  return lines.join("\n");
}
