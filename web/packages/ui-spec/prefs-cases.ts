// The saved-settings cases (prefs-cases.json), typed, and the helper both of this package's runners
// use. Kept out of `index.ts` so the portal's bundle doesn't pull the cases in.
import casesJson from "./prefs-cases.json";
import { PREFS, type PrefGroupName } from "./prefs";
import type { Platform } from "./index";

export interface PrefsCase {
  name: string;
  group: PrefGroupName;
  /** Absent means both. */
  platforms?: Platform[];
  /** The saved text, as stored (malformed, empty, or null for nothing saved). Exactly one of `text` and `value`. */
  text?: string | null;
  /** The saved JSON value, written to storage as JSON text. */
  value?: unknown;
  /** The fields that differ from the group's defaults after parsing; a field left out is its default (or absent when optional). Fields the platform doesn't keep are ignored. */
  expect: Record<string, unknown>;
}

const file = casesJson as unknown as { defaults: Record<PrefGroupName, Record<string, unknown>>; cases: PrefsCase[] };

export const PREFS_CASES = file.cases;
/** What each group holds with nothing saved, written out here and not read from prefs.json, so changing a default there fails the cases. */
export const PREFS_DEFAULTS = file.defaults;

/** The text a case stores. */
export const savedText = (c: PrefsCase): string | null => ("text" in c ? (c.text ?? null) : JSON.stringify(c.value));

export const runsOn = (c: PrefsCase, platform: Platform): boolean => !c.platforms || c.platforms.includes(platform);

/** What the case expects a platform to hold: the pinned defaults, the case's fields over them, and only the fields the platform keeps. */
export function expectedPrefs(c: PrefsCase, platform: Platform): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [name, value] of Object.entries({ ...PREFS_DEFAULTS[c.group], ...c.expect })) {
    const f = PREFS[c.group].fields[name];
    if (f && (!f.platforms || f.platforms.includes(platform))) out[name] = structuredClone(value);
  }
  return out;
}
