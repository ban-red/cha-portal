// The saved settings' schema (prefs.json) and the one validator both of this package's users run
// (ADR 0016). `crates/cha-ui-spec/src/prefs.rs` is its Rust twin; prefs-cases.json keeps them equal.
import prefsJson from "./prefs.json";
import type { Platform } from "./index";

export type PrefType = "bool" | "int" | "number" | "string" | "enum" | "list" | "object";

export interface PrefField {
  type: PrefType;
  doc: string;
  /** The value when the field is missing or invalid. Absent for an optional field. */
  default?: unknown;
  /** Optional: absent from the result until it is saved well formed. */
  optional?: boolean;
  /** Who keeps the field; both when absent. */
  platforms?: Platform[];
  min?: number;
  max?: number;
  /** An int takes a fractional number by rounding a half up. */
  round?: boolean;
  /** A number outside min..max is moved to the nearest limit instead of being invalid. */
  clamp?: boolean;
  /** string: not empty. */
  nonempty?: boolean;
  /** enum: the allowed values; list: the allowed entries, in their order. */
  values?: string[];
  /** object: its fields, all required. */
  fields?: Record<string, PrefField>;
}

export interface PrefGroup {
  doc: string;
  fields: Record<string, PrefField>;
}

export type PrefGroupName = "stats_panel" | "toolbar";

export const PREFS = prefsJson as unknown as Record<PrefGroupName, PrefGroup>;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/** One value against its field: the value to keep, or undefined when it is not valid. */
export function parsePrefValue(field: PrefField, v: unknown): unknown {
  switch (field.type) {
    case "bool":
      return typeof v === "boolean" ? v : undefined;
    case "string":
      return typeof v === "string" && (!field.nonempty || v) ? v : undefined;
    case "enum":
      return typeof v === "string" && field.values!.includes(v) ? v : undefined;
    case "list":
      return Array.isArray(v) ? field.values!.filter((id) => v.includes(id)) : undefined;
    case "object": {
      if (!isObj(v)) return undefined;
      const out: Record<string, unknown> = {};
      for (const [name, sub] of Object.entries(field.fields!)) {
        const kept = parsePrefValue(sub, v[name]);
        if (kept === undefined) return undefined;
        out[name] = kept;
      }
      return out;
    }
    case "int":
    case "number": {
      if (typeof v !== "number" || !Number.isFinite(v)) return undefined;
      let n = v;
      if (field.type === "int") {
        if (field.round) n = Math.floor(n + 0.5);
        else if (!Number.isInteger(n)) return undefined;
      }
      if (field.clamp) {
        if (field.min !== undefined) n = Math.max(field.min, n);
        if (field.max !== undefined) n = Math.min(field.max, n);
      } else if ((field.min !== undefined && n < field.min) || (field.max !== undefined && n > field.max)) return undefined;
      return n;
    }
  }
}

/** A fresh copy of a default, so callers can't share one array or object. */
const fresh = (v: unknown): unknown => (v === undefined ? undefined : JSON.parse(JSON.stringify(v)));

/**
 * A group's saved settings from parsed JSON: the fields this platform keeps, each valid or its
 * default (an optional field with none left out). Anything that is not an object gives the defaults.
 */
export function parsePrefsGroup(group: PrefGroupName, input: unknown, platform: Platform): Record<string, unknown> {
  const o = isObj(input) ? input : {};
  const out: Record<string, unknown> = {};
  for (const [name, field] of Object.entries(PREFS[group].fields)) {
    if (field.platforms && !field.platforms.includes(platform)) continue;
    const kept = parsePrefValue(field, o[name]);
    const value = kept !== undefined ? kept : fresh(field.default);
    if (value !== undefined) out[name] = value;
  }
  return out;
}

/** The same from JSON text; missing storage (null) or text that does not parse gives the defaults. */
export function parsePrefsText(group: PrefGroupName, raw: string | null, platform: Platform): Record<string, unknown> {
  let input: unknown = null;
  if (raw) {
    try {
      input = JSON.parse(raw);
    } catch {
      // the defaults
    }
  }
  return parsePrefsGroup(group, input, platform);
}

/** What a group holds with nothing saved. */
export const prefDefaults = (group: PrefGroupName, platform: Platform) => parsePrefsGroup(group, null, platform);
