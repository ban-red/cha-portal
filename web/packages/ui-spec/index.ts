// Types for the spec's JSON files (ADR 0016). `crates/cha-ui-spec` reads the same files.
import healthJson from "./health.json";
import iconsJson from "./icons.json";

export * from "./panel";
export * from "./prefs";
export * from "./toolbar";

/** One drawn part of an icon: an SVG path, stroked at `stroke` units wide, or filled. */
export type IconPart = { d: string; stroke: number; fill?: undefined } | { d: string; fill: true; stroke?: undefined };

export interface IconSpec {
  /** The square the path data is drawn in; always 16. */
  viewBox: number;
  parts: IconPart[];
  /** Stroke ends. Absent means the SVG default (butt). */
  linecap?: "round" | "butt" | "square";
  /** Stroke corners. Absent means the SVG default (miter). */
  linejoin?: "round" | "bevel" | "miter";
}

export const ICONS = iconsJson as Record<string, IconSpec>;
export type IconName = keyof typeof iconsJson;

export function hasIcon(id: string): boolean {
  return Object.hasOwn(ICONS, id);
}

// --- health.json: the grade's constants, bands, weights and issue texts ---

export type Platform = "web" | "native";

/** A text both platforms share, or each platform's own wording (a platform may be left out where it has none). */
export type PlatformText = string | { web?: string; native?: string };

export function pickText(text: PlatformText, platform: Platform): string | undefined {
  return typeof text === "string" ? text : text[platform];
}

/** The first value that counts and the value that is as bad as it gets (`from` above `to` for a signal where lower is worse). */
export interface Band {
  from: number;
  to: number;
}

export type HealthSeverity = "minor" | "major" | "critical";
export type HealthGradeLetter = "A" | "B" | "C" | "D" | "F";

/** What a template's placeholder holds: a number (written `{name:formatter}`) or a string (`{name}`). */
export type PlaceholderKind = "number" | "string";

export interface HealthIssueSpec {
  id: string;
  platforms: Platform[];
  title: PlatformText;
  /** The one-line summary words while this is the worst issue. */
  summary: string;
  /** Score points it costs at level 1; at lower levels, in proportion. */
  weight: number;
  hint: PlatformText;
  /** Detail templates by variant. Most issues have one, `main`. A variant may exist for one platform only. */
  details: Record<string, PlatformText>;
  /** Every placeholder the issue's detail templates use. */
  placeholders: Record<string, PlaceholderKind>;
}

export interface HealthSpec {
  /** Snapshots judged: the last ~8 s. */
  window: number;
  min_snapshots: number;
  /** The frame rate assumed until the streamer says. */
  fallback_fps: number;
  levels: { base: number; slope: number; spike_share: number; min: number; major: number; critical: number };
  severity_cap: Record<HealthSeverity, number>;
  /** Highest first. */
  grades: { grade: HealthGradeLetter; floor: number }[];
  text: { hidden: string; measuring: string; smooth: string; list_separator: string };
  params: {
    busy_sent_fps: number;
    gpu_late_encode: number;
    freeze_budgets: number;
    freeze_min_ms: number;
    freeze_to_ms: number;
    no_frames_gap_ms: number;
    latency_spread_samples: number;
    sound_present: number;
    sound_played: number;
    sound_out_seconds: number;
    sound_restart_first: number;
    sound_restart_step: number;
    sound_out_first: number;
    sound_out_step: number;
  };
  bands: {
    fps_ratio: Band;
    dropped: Band;
    decode_budget: Band;
    latency: Band;
    delivery_spread: Band;
    latency_spread: Band;
    jitter_buffer: Band;
    skipped: Band;
    lost_frames: Band;
    lost_packets: Band;
    recovered: Band;
    partial: Band;
    rtt: Band;
    node_cpu: Band;
    node_ram: Band;
    node_gpu: Band;
    node_vram: Band;
    node_enc: Band;
    streamer_cpu: Band;
    audio_jitter_wt: Band;
    audio_jitter_rtc: Band;
  };
  /** Why the numbers are what they are; not read by code. */
  notes: Record<string, string>;
  /** In the order the checks run (it breaks ties between equal issues). */
  issues: HealthIssueSpec[];
}

export const HEALTH = healthJson as unknown as HealthSpec;

/** The issues a platform can report, in spec order. */
export const healthIssues = (platform: Platform): HealthIssueSpec[] => HEALTH.issues.filter((i) => i.platforms.includes(platform));

/** A text for a platform; throws when the spec has none, which is a spec bug the tests catch. */
export function textFor(text: PlatformText, platform: Platform, what = "text"): string {
  const t = pickText(text, platform);
  if (t === undefined) throw new Error(`health.json: no ${what} for ${platform}`);
  return t;
}

// --- templates ---

export type FillValue = number | string;

/** `Number.prototype.toFixed`: a half rounds up (away from zero), and a NaN reads "NaN". The Rust side matches it. */
export const fixed = (v: number, digits: number): string => v.toFixed(digits);

/**
 * The formatters a placeholder can name: `{name:ms1}`. A number needs one; a string takes none.
 * `f0`..`f2`: that many decimals. `ms0`, `ms1`: the same, then " ms". `int`: the whole part, with no
 * digit grouping. `gb`: bytes as GiB with one decimal. `pct`: a whole number and "%". `mbit`: one
 * decimal and " Mbit/s". `s`: the plural "s" (nothing for exactly 1), for `reconnect{n:s}`.
 */
export const FORMATTERS: Record<string, (v: number) => string> = {
  f0: (v) => fixed(v, 0),
  f1: (v) => fixed(v, 1),
  f2: (v) => fixed(v, 2),
  ms0: (v) => `${fixed(v, 0)} ms`,
  ms1: (v) => `${fixed(v, 1)} ms`,
  int: (v) => String(Math.trunc(v)),
  gb: (v) => fixed(v / 1024 ** 3, 1),
  pct: (v) => `${fixed(v, 0)}%`,
  mbit: (v) => `${fixed(v, 1)} Mbit/s`,
  s: (v) => (v === 1 ? "" : "s"),
};

/** What a formatter writes after the number, so the stats panel can write "– ms" for a number it lacks. */
export const UNITS: Record<string, string> = { ms0: " ms", ms1: " ms", pct: "%", mbit: " Mbit/s" };

/** A placeholder: `{name}` or `{name:formatter}`. Anything else in braces stays as written. */
export const PLACEHOLDER = /\{([a-z][a-z0-9_]*)(?::([a-z0-9]+))?\}/g;

/** Fills a template's placeholders. A missing value, an unknown formatter, a number without one or a string with one throws. */
export function fill(template: string, values: Record<string, FillValue>): string {
  return template.replace(PLACEHOLDER, (_, name: string, fmt: string | undefined) => {
    const v = values[name];
    if (v === undefined) throw new Error(`fill: no value for {${name}} in "${template}"`);
    if (typeof v === "string") {
      if (fmt) throw new Error(`fill: {${name}:${fmt}} is a string and takes no formatter`);
      return v;
    }
    if (!fmt) throw new Error(`fill: {${name}} is a number and needs a formatter`);
    const f = FORMATTERS[fmt];
    if (!f) throw new Error(`fill: unknown formatter ${fmt} in "${template}"`);
    return f(v);
  });
}
