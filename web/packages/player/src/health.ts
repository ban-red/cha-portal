// A letter grade for the stream right now, from the last few seconds of stats snapshots.

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

/** Snapshots judged: the last ~8 s. Long enough that one hiccup doesn't flash an F, short enough that a fix shows. */
export const HEALTH_WINDOW = 8;
/** Fewer snapshots than this and the counters' deltas and the spread mean nothing. */
export const MIN_SNAPSHOTS = 2;

/**
 * A snapshot's level for a signal is 0 below `from`, then 0.2 at `from` rising linearly to 1 at `to`.
 * The window's level is the larger of its mean (sustained trouble) and `SPIKE_SHARE` of its worst
 * (a spike counts, but a single one stays minor).
 */
export const SPIKE_SHARE = 0.35;
/** A signal whose window level is under this isn't reported. */
export const MIN_LEVEL = 0.1;
/** Window levels from which an issue is major, and critical. */
export const MAJOR_LEVEL = 0.4;
export const CRITICAL_LEVEL = 0.75;

/** The best score an issue of each severity allows: any issue rules out an A, a major one a B, a critical one a C. */
export const SEVERITY_CAP: Record<IssueSeverity, number> = { minor: 89, major: 79, critical: 69 };
/** Score floors of the letters. */
export const GRADE_FLOORS: [HealthGrade, number][] = [
  ["A", 90],
  ["B", 80],
  ["C", 70],
  ["D", 55],
  ["F", 0],
];

// Thresholds are the first value that counts (`from`) and the value that is as bad as it gets (`to`),
// set for a wired or good Wi-Fi LAN, the product's target.

/** Shown fps as a fraction of the fps the streamer sent: under 95% is visible stutter, under 60% a slideshow. */
const FPS_RATIO = { from: 0.95, to: 0.6 };
/**
 * The streamer sends only when the picture changes, so an idle desktop sends almost nothing and
 * shows almost nothing. Stutter and freezes are judged only while it sends at least this many
 * frames per second; without a send rate (an older streamer) they aren't judged at all.
 */
const BUSY_SENT_FPS = 10;
/** Frames the browser dropped, per second as a fraction of the target rate. */
const DROPPED = { from: 0.01, to: 0.15 };
/** Decode time as a fraction of the frame budget (1000 / fps): hardware decoders pipeline, but past half a frame there is no headroom. */
const DECODE_BUDGET = { from: 0.5, to: 1.2 };
/** Send → shown p50, ms: under 20 is A on a LAN. */
const LATENCY = { from: 20, to: 100 };
/** Delivery p95 over p50, ms, and the spread of the latency p50 across the window (interquartile range, ms). */
const DELIVERY_SPREAD = { from: 8, to: 40 };
const LATENCY_SPREAD = { from: 6, to: 30 };
/** WebRTC jitter-buffer wait per frame, ms. */
const JITTER_BUFFER = { from: 25, to: 80 };
/** Lost frames per second (WebTransport counts frames FEC couldn't rebuild) or packets per second (WebRTC, which retransmits). */
const LOST_FRAMES = { from: 0.2, to: 6 };
const LOST_PACKETS = { from: 2, to: 60 };
/** Frames FEC rebuilt per second: no harm, but the network is dropping packets. */
const RECOVERED = { from: 2, to: 30 };
/** Round trip, ms: a LAN is a few. */
const RTT = { from: 10, to: 80 };
/** The longest wait between frames, in frame budgets (at least this many ms), and when it is a freeze. */
const FREEZE_BUDGETS = 3;
const FREEZE = { to: 500 };
/** The node's use, percent. */
const NODE_CPU = { from: 90, to: 100 };
const NODE_RAM = { from: 95, to: 100 };
const NODE_GPU = { from: 95, to: 100 };
const NODE_VRAM = { from: 95, to: 100 };
const NODE_ENC = { from: 95, to: 100 };
/** The streamer's CPU as a share of the whole machine, or of one core when the core count is unknown. */
const STREAMER_CPU = { from: 90, to: 100 };
/** Audio jitter-buffer wait, ms: NetEq is kept near its minimum (tens of ms), so growth means late packets. */
const AUDIO_JITTER = { from: 40, to: 200 };

/** Score points an issue costs at level 1; at lower levels, in proportion. */
const WEIGHT = {
  stutter: 30,
  dropped: 20,
  decode: 35,
  latency: 30,
  jitter: 15,
  loss: 40,
  recovered: 8,
  rtt: 12,
  freeze: 45,
  node: 30,
  audio: 10,
};

const FALLBACK_FPS = 60;

interface Band {
  from: number;
  to: number;
}

function level(v: number | null | undefined, band: Band): number {
  if (v === null || v === undefined || !Number.isFinite(v) || v < band.from) return 0;
  return 0.2 + 0.8 * Math.min(1, (v - band.from) / (band.to - band.from));
}

/** The same for a signal where lower is worse (`from` above `to`). */
function levelBelow(v: number | null | undefined, band: Band): number {
  if (v === null || v === undefined || !Number.isFinite(v) || v > band.from) return 0;
  return 0.2 + 0.8 * Math.min(1, (band.from - v) / (band.from - band.to));
}

/** What a check found: the window's level, and the text for it. */
interface Finding {
  level: number;
  detail: string;
}

interface Check {
  id: string;
  title: string;
  /** For the one-line summary. */
  summary: string;
  weight: number;
  hint: string;
  /** A snapshot's level, for the checks judged sample by sample; the check's own `detail` text is built from `worst`. */
  find(window: StatsSnapshot[], ctx: Ctx): Finding | null;
}

interface Ctx {
  /** The streamer's frame rate, or the fallback until it says (for the frame budget and drop rate). */
  target: number;
  budgetMs: number;
  seconds: number;
}

const ms = (v: number, digits = 0) => `${v.toFixed(digits)} ms`;
const mean = (a: number[]) => a.reduce((x, y) => x + y, 0) / a.length;

/** Mean and worst of per-snapshot levels, combined as described at `SPIKE_SHARE`. */
function windowLevel(levels: number[]): number {
  if (!levels.length) return 0;
  return Math.max(mean(levels), SPIKE_SHARE * Math.max(...levels));
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

/** Whether the stream is WebTransport, which has delivery times and frame gaps that WebRTC lacks. */
const isWebTransport = (window: StatsSnapshot[]) => window.some((s) => s.deliveryMs !== null || s.frameGapMs !== null);

const CHECKS: Check[] = [
  {
    id: "stutter",
    title: "Stuttering picture",
    summary: "Some stutter",
    weight: WEIGHT.stutter,
    hint: "Frames are being sent but not all of them show. Check the other lines: if the network is fine, try 60 fps or another codec; if it isn't, a cable beats Wi-Fi.",
    find(window) {
      const busy = window.filter((s) => s.sentFps !== null && s.sentFps >= BUSY_SENT_FPS && s.fps !== null);
      const l = windowLevel(busy.map((s) => levelBelow(s.fps! / s.sentFps!, FPS_RATIO)));
      if (l === 0) return null;
      return { level: l, detail: `${mean(busy.map((s) => s.fps!)).toFixed(0)} fps shown of ${mean(busy.map((s) => s.sentFps!)).toFixed(0)} sent` };
    },
  },
  {
    id: "dropped",
    title: "Browser dropping frames",
    summary: "Dropped frames",
    weight: WEIGHT.dropped,
    hint: "The browser decoded frames it had no time to show. Close heavy tabs and apps on this computer, or try 60 fps.",
    find(window, { target, seconds }) {
      if (seconds <= 0) return null;
      const rate = growth(window, (s) => s.framesDropped) / seconds;
      return rateFinding(rate / target, DROPPED, () => `${rate.toFixed(1)} frames/s dropped`);
    },
  },
  {
    id: "decode",
    title: "Slow decoding",
    summary: "Slow decoding",
    weight: WEIGHT.decode,
    hint: "Your browser or computer can't decode fast enough. Try 60 fps or another codec (HEVC and H.264 are hardware-decoded on most machines), and close other heavy apps.",
    find(window, { budgetMs }) {
      // Only while frames come at a rate: a few frames on a still picture
      // (often keyframes, the slowest to decode) hold nothing up.
      const busy = window.filter((s) => s.sentFps !== null && s.sentFps >= BUSY_SENT_FPS);
      const v = values(busy, (s) => s.decodeMs);
      const l = windowLevel(v.map((d) => level(d / budgetMs, DECODE_BUDGET)));
      if (l === 0) return null;
      return { level: l, detail: `${ms(mean(v), 1)} to decode a frame, of ${ms(budgetMs, 1)} per frame` };
    },
  },
  {
    id: "freeze",
    title: "Picture freezes",
    summary: "Freezing",
    weight: WEIGHT.freeze,
    hint: "No frames arrived for a moment while the node was sending. Usually a network stall (Wi-Fi roaming or interference, a busy link) or a node that stopped to catch up; a cable is the first thing to try.",
    find(window, { budgetMs }) {
      const levels: number[] = [];
      let worst = 0;
      for (const s of window) {
        // Only while frames were being sent: a still picture has long gaps by nature. A source
        // sending at 10 fps has 100 ms between frames, so the gap must also beat 3 send intervals.
        if (s.sentFps === null || s.sentFps < BUSY_SENT_FPS) continue;
        // A WebRTC page has no gap measure, but a second with no frames at all is one.
        const gap = s.frameGapMs !== null ? s.frameGapMs : s.fps === 0 ? 1000 : null;
        if (gap === null) continue;
        const from = Math.max(50, FREEZE_BUDGETS * budgetMs, (FREEZE_BUDGETS * 1000) / s.sentFps);
        levels.push(level(gap, { from, to: FREEZE.to }));
        worst = Math.max(worst, gap);
      }
      const l = windowLevel(levels);
      if (l === 0) return null;
      return { level: l, detail: `up to ${ms(worst)} between frames` };
    },
  },
  {
    id: "latency",
    title: "High latency",
    summary: "High latency",
    weight: WEIGHT.latency,
    hint: "Frames take long from the node to your screen. Check the network (a cable, not Wi-Fi) and whether the node is busy; a lower frame rate or bitrate can help on a weak link.",
    find(window) {
      const v = values(window, (s) => s.latencyMs);
      const l = windowLevel(v.map((d) => level(d, LATENCY)));
      if (l === 0) return null;
      return { level: l, detail: `${ms(mean(v))} from sent to shown` };
    },
  },
  {
    id: "jitter",
    title: "Uneven latency",
    summary: "Uneven latency",
    weight: WEIGHT.jitter,
    hint: "Delivery time varies from frame to frame, which shows as judder. Queueing on the network (other traffic, Wi-Fi) is the usual cause.",
    find(window) {
      const candidates: Finding[] = [];
      const spreads = values(window, (s) => (s.deliveryMs !== null && s.deliveryP95Ms !== null ? s.deliveryP95Ms - s.deliveryMs : null));
      if (spreads.length) {
        const l = windowLevel(spreads.map((d) => level(d, DELIVERY_SPREAD)));
        if (l > 0) candidates.push({ level: l, detail: `delivery varies by ${ms(Math.max(...spreads))} (p95 over p50)` });
      }
      const lat = values(window, (s) => s.latencyMs);
      if (lat.length >= 3) {
        const sd = interquartile(lat);
        const l = level(sd, LATENCY_SPREAD);
        if (l > 0) candidates.push({ level: l, detail: `latency wanders by ${ms(sd)} over the last seconds` });
      }
      const buf = values(window, (s) => s.jitterMs);
      if (buf.length) {
        const l = windowLevel(buf.map((d) => level(d, JITTER_BUFFER)));
        if (l > 0) candidates.push({ level: l, detail: `jitter buffer holds frames ${ms(mean(buf))}` });
      }
      return candidates.sort((a, b) => b.level - a.level)[0] ?? null;
    },
  },
  {
    id: "loss",
    title: "Packet loss",
    summary: "Network losses",
    weight: WEIGHT.loss,
    hint: "The network is dropping packets, which costs frames or forces resends. Wi-Fi interference or a congested link: try a cable, or move closer to the access point.",
    find(window, { seconds }) {
      if (seconds <= 0) return null;
      const wt = isWebTransport(window);
      const rate = growth(window, (s) => s.packetsLost) / seconds;
      return rateFinding(rate, wt ? LOST_FRAMES : LOST_PACKETS, () => `${rate.toFixed(1)} ${wt ? "frames" : "packets"} lost per second`);
    },
  },
  {
    id: "recovered",
    title: "Network needs repair",
    summary: "Lossy network",
    weight: WEIGHT.recovered,
    hint: "Error correction is rebuilding frames that lost packets, so you see nothing yet, but the link is dropping data and a worse moment would show. Wi-Fi or a busy link: try a cable.",
    find(window, { seconds }) {
      if (seconds <= 0) return null;
      const rate = growth(window, (s) => s.framesRecovered) / seconds;
      return rateFinding(rate, RECOVERED, () => `${rate.toFixed(1)} frames/s rebuilt from parity`);
    },
  },
  {
    id: "rtt",
    title: "Slow network round trip",
    summary: "Slow network",
    weight: WEIGHT.rtt,
    hint: "The round trip to the node is long for a LAN. Check you are on the same network as the node, and for a VPN or Wi-Fi in the way.",
    find(window) {
      const v = values(window, (s) => s.rttMs);
      const l = windowLevel(v.map((d) => level(d, RTT)));
      if (l === 0) return null;
      return { level: l, detail: `round trip ${ms(mean(v), 1)}` };
    },
  },
  {
    id: "node",
    title: "Node overloaded",
    summary: "Node overloaded",
    weight: WEIGHT.node,
    hint: "The machine running the environment is out of something, so frames are made late. Close other apps on it, lower the frame rate, or choose another device.",
    find(window) {
      const nodes = values(window, (s) => s.node);
      if (!nodes.length) return null;
      const per = nodes.map(nodeLevels);
      const l = windowLevel(per.map((p) => Math.max(0, ...p.map((x) => x.level))));
      if (l === 0) return null;
      // Name whatever was over in the worst reading.
      const worst = per.reduce((a, b) => (Math.max(0, ...b.map((x) => x.level)) > Math.max(0, ...a.map((x) => x.level)) ? b : a));
      return { level: l, detail: worst.filter((x) => x.level > 0).map((x) => x.text).join(", ") };
    },
  },
  {
    id: "audio",
    title: "Sound buffering",
    summary: "Sound hiccups",
    weight: WEIGHT.audio,
    hint: "Audio packets are arriving late, so sound may crackle or lag. It follows the network: a cable helps.",
    find(window) {
      const v = values(window, (s) => s.audioJitterMs);
      const l = windowLevel(v.map((d) => level(d, AUDIO_JITTER)));
      if (l === 0) return null;
      return { level: l, detail: `audio buffer at ${ms(mean(v))}` };
    },
  },
];

/** What a node report puts over its limits, each as a level and a phrase. */
function nodeLevels(n: NodeStats): { level: number; text: string }[] {
  const pct = (v: number) => `${v.toFixed(0)}%`;
  const out = [
    { level: level(n.cpu, NODE_CPU), text: `CPU ${pct(n.cpu)}` },
    { level: n.memTotal > 0 ? level((n.memUsed / n.memTotal) * 100, NODE_RAM) : 0, text: `RAM ${pct((n.memUsed / n.memTotal) * 100)}` },
    { level: level(n.gpu, NODE_GPU), text: `GPU ${pct(n.gpu ?? 0)}` },
    {
      level: n.vramTotal && n.vramUsed !== undefined ? level((n.vramUsed / n.vramTotal) * 100, NODE_VRAM) : 0,
      text: `VRAM ${pct(((n.vramUsed ?? 0) / (n.vramTotal || 1)) * 100)}`,
    },
    { level: level(n.enc, NODE_ENC), text: `NVENC ${pct(n.enc ?? 0)}` },
    { level: level(n.cores > 0 ? n.streamerCpu / n.cores : n.streamerCpu, STREAMER_CPU), text: `streamer CPU ${pct(n.streamerCpu)} of a core` },
  ];
  return out;
}

const severityOf = (l: number): IssueSeverity => (l >= CRITICAL_LEVEL ? "critical" : l >= MAJOR_LEVEL ? "major" : "minor");
const SEVERITY_RANK: Record<IssueSeverity, number> = { critical: 2, major: 1, minor: 0 };

/**
 * Grades the stream from `history` (oldest first; only the last `HEALTH_WINDOW` count).
 * Fields a snapshot lacks (an older streamer, or the other transport) are skipped, never penalised.
 */
export function assessHealth(history: StatsSnapshot[], context: HealthContext): HealthAssessment {
  if (!context.visible) return { grade: null, score: null, summary: "Paused while the tab is hidden", issues: [] };
  const window = history.slice(-HEALTH_WINDOW);
  if (window.length < MIN_SNAPSHOTS) return { grade: null, score: null, summary: "Measuring…", issues: [] };

  const latest = window[window.length - 1]!;
  const target = latest.targetFps && latest.targetFps > 0 ? latest.targetFps : FALLBACK_FPS;
  const ctx: Ctx = {
    target,
    budgetMs: 1000 / target,
    seconds: ((window.length - 1) * (context.intervalMs ?? 1000)) / 1000,
  };

  const found: { issue: HealthIssue; penalty: number; summary: string }[] = [];
  for (const check of CHECKS) {
    const f = check.find(window, ctx);
    if (!f || f.level < MIN_LEVEL) continue;
    found.push({
      issue: { id: check.id, severity: severityOf(f.level), title: check.title, detail: f.detail, hint: check.hint },
      penalty: check.weight * f.level,
      summary: check.summary,
    });
  }
  found.sort((a, b) => SEVERITY_RANK[b.issue.severity] - SEVERITY_RANK[a.issue.severity] || b.penalty - a.penalty);

  let score = 100 - found.reduce((sum, f) => sum + f.penalty, 0);
  for (const f of found) score = Math.min(score, SEVERITY_CAP[f.issue.severity]);
  score = Math.max(0, Math.round(score));
  const grade = GRADE_FLOORS.find(([, floor]) => score >= floor)![0];
  return {
    grade,
    score,
    summary: found.length ? found[0]!.summary : "Smooth",
    issues: found.map((f) => f.issue),
  };
}
