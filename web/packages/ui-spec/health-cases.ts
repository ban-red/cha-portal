// The shared health test vectors (ADR 0016), typed, and the history expander both players' runners use.
// Kept out of `index.ts` so the portal's bundle doesn't pull the cases in.
import casesJson from "./health-cases.json";
import type { Platform } from "./index";

/** A measurement. "NaN" stands for a NaN, which JSON can't hold. */
export type SpecNum = number | "NaN";

/** The node's resource use, as the streamer reports it. Memory is in any unit, as long as used and total agree. */
export interface SpecNode {
  cpu?: SpecNum | null;
  cores?: SpecNum | null;
  mem_used?: SpecNum | null;
  mem_total?: SpecNum | null;
  gpu?: SpecNum | null;
  vram_used?: SpecNum | null;
  vram_total?: SpecNum | null;
  enc?: SpecNum | null;
  streamer_cpu?: SpecNum | null;
}

/**
 * One second of a stream, in the shape both players can fill. Every field is optional and means
 * "unknown" when missing or null; a player skips what it can't measure.
 */
export interface SpecSnapshot {
  /** The codec family, lower case: "hevc", "h264", "av1", "pyrowave444". */
  codec?: string | null;
  /** The frame rate the streamer encodes at. */
  target_fps?: SpecNum | null;
  /** Frames per second the streamer sent, and the same span's frames shown here. */
  sent_fps?: SpecNum | null;
  shown_sent_fps?: SpecNum | null;
  /** Frames per second put on screen over the last second. */
  shown_fps?: SpecNum | null;
  /** The streamer's composited -> encoded p99, ms. */
  encode_p99_ms?: SpecNum | null;
  decode_ms?: SpecNum | null;
  /** Web only: the jitter buffer's wait per frame. */
  jitter_buffer_ms?: SpecNum | null;
  rtt_ms?: SpecNum | null;
  /** Cumulative counters since the session began; null when the transport can't count them. */
  lost?: SpecNum | null;
  recovered?: SpecNum | null;
  partial?: SpecNum | null;
  dropped?: SpecNum | null;
  /** Send -> shown on the web, received -> shown natively. */
  latency_ms?: SpecNum | null;
  /** Web only: send -> decoded p50 and p95. */
  delivery_ms?: SpecNum | null;
  delivery_p95_ms?: SpecNum | null;
  /** The longest wait between two frames over the last second. */
  frame_gap_ms?: SpecNum | null;
  /** Web only. */
  audio_jitter_ms?: SpecNum | null;
  audio_restarts?: SpecNum | null;
  audio_in_peak?: SpecNum | null;
  audio_out_peak?: SpecNum | null;
  node?: SpecNode | null;
  /** Native only: arrival gaps typical of AWDL were seen. */
  awdl_suspected?: boolean | null;
}

/**
 * A run of snapshots: the file's `base` with these fields set. `repeat` (default 1) says how many;
 * a field may be an array of `repeat` values, one per snapshot. `node` merges into the base's node
 * field by field (null removes a field), and `node: null` is no report at all.
 */
export type HistoryEntry = { repeat?: number } & { [K in keyof SpecSnapshot]?: SpecSnapshot[K] | SpecNum[] };

export interface SpecContext {
  /** Default true. Only the browser has a hidden state. */
  visible?: boolean;
  /** Default 1000. Only the browser takes another interval. */
  interval_ms?: number;
}

export type ExpectedText = string | { web?: string; native?: string };

export interface HealthCase {
  name: string;
  /** Absent means both. */
  platforms?: Platform[];
  history: HistoryEntry[];
  context?: SpecContext;
  expect: {
    grade: "A" | "B" | "C" | "D" | "F" | null;
    score: number | null;
    summary: string;
    /** Exactly these, in this order. */
    issues: { id: string; severity: "minor" | "major" | "critical" }[];
    /** Exact detail strings by issue id; a platform's own wording where they differ. */
    details?: Record<string, ExpectedText>;
    /** Exact hint strings by issue id. */
    hints?: Record<string, ExpectedText>;
  };
}

export interface HealthCases {
  /** Every history entry starts from this: a healthy 60 fps LAN stream. */
  base: SpecSnapshot;
  cases: HealthCase[];
}

export const HEALTH_CASES = casesJson as unknown as HealthCases;

/** The text a case expects on a platform. */
export function expectedText(t: ExpectedText, platform: Platform): string | undefined {
  return typeof t === "string" ? t : t[platform];
}

/** Whether a case runs on a platform. */
export const runsOn = (c: HealthCase, platform: Platform): boolean => !c.platforms || c.platforms.includes(platform);

/** The snapshots a case's history stands for, oldest first. */
export function expandHistory(base: SpecSnapshot, entries: HistoryEntry[]): SpecSnapshot[] {
  const out: SpecSnapshot[] = [];
  for (const entry of entries) {
    const repeat = entry.repeat ?? 1;
    for (let i = 0; i < repeat; i++) {
      const snap: Record<string, unknown> = { ...base, node: base.node ? { ...base.node } : base.node };
      for (const [key, value] of Object.entries(entry)) {
        if (key === "repeat") continue;
        let v: unknown = value;
        if (Array.isArray(v)) {
          if (v.length !== repeat) throw new Error(`history field ${key}: ${v.length} values for repeat ${repeat}`);
          v = v[i];
        }
        if (key === "node" && v !== null && typeof v === "object") {
          snap.node = { ...(base.node ?? {}), ...(v as object) };
        } else {
          snap[key] = v;
        }
      }
      out.push(snap as SpecSnapshot);
    }
  }
  return out;
}
