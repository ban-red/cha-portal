// Applies the theme preferences to <html>. The inline script in index.html does the same
// resolution before first paint: the two MUST agree (same keys, same "system" rules), so
// change both together.
import { computed, readonly, ref } from "vue";

import { DEFAULT_PREFS, MAX_PINNED, THEMES, type ThemeId, type ThemePrefs, type UserPrefs } from "./index";

export const STORAGE_KEY = "cha.theme";

const APPEARANCES = ["system", "dark", "light"] as const;
const CONTRASTS = ["system", "standard", "more"] as const;
const MOTIONS = ["system", "reduced"] as const;
const TRANSPARENCIES = ["system", "reduced"] as const;

function pick<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === "string" && (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
}

const TEMPLATE_ID = /^[a-z0-9-]{1,40}$/;

/** Unique, well-formed template ids, at most MAX_PINNED; anything else is dropped. */
function parsePinned(v: unknown): string[] {
  if (!Array.isArray(v)) return [];
  const ids = v.filter((x): x is string => typeof x === "string" && TEMPLATE_ID.test(x));
  return [...new Set(ids)].slice(0, MAX_PINNED);
}

/** Any value to valid prefs, field by field: unknown or invalid values take the default. */
export function parsePrefs(raw: unknown): UserPrefs {
  const o = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    theme: pick(
      o.theme,
      THEMES.map((t) => t.id),
      DEFAULT_PREFS.theme,
    ),
    appearance: pick(o.appearance, APPEARANCES, DEFAULT_PREFS.appearance),
    contrast: pick(o.contrast, CONTRASTS, DEFAULT_PREFS.contrast),
    motion: pick(o.motion, MOTIONS, DEFAULT_PREFS.motion),
    transparency: pick(o.transparency, TRANSPARENCIES, DEFAULT_PREFS.transparency),
    pinned: parsePinned(o.pinned),
    envView: pick(o.envView, ["grid", "list"] as const, DEFAULT_PREFS.envView),
    envSort: pick(o.envSort, ["name", "recent"] as const, DEFAULT_PREFS.envSort),
  };
}

export function loadPrefs(): UserPrefs {
  try {
    const s = localStorage.getItem(STORAGE_KEY);
    return parsePrefs(s ? JSON.parse(s) : null);
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(prefs: UserPrefs): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Private mode or storage blocked: the choice just won't outlive the tab.
  }
}

// ---- resolution --------------------------------------------------------------------------

export interface SystemEnv {
  /** prefers-color-scheme: light. With no preference we stay dark, like the portal always was. */
  light: boolean;
  /** prefers-contrast: more */
  moreContrast: boolean;
  /** prefers-reduced-motion: reduce */
  reducedMotion: boolean;
  /** prefers-reduced-transparency: reduce */
  reducedTransparency: boolean;
}

export interface Resolved {
  theme: ThemeId;
  appearance: "dark" | "light";
  contrast: "standard" | "more";
  motion: "full" | "reduced";
  transparency: "full" | "reduced";
}

type MatchMedia = (query: string) => { matches: boolean };

export function readSystemEnv(mm: MatchMedia | undefined = globalThis.matchMedia?.bind(globalThis)): SystemEnv {
  const m = (q: string) => {
    try {
      return !!mm && mm(q).matches;
    } catch {
      return false;
    }
  };
  return {
    light: m("(prefers-color-scheme: light)"),
    moreContrast: m("(prefers-contrast: more)"),
    reducedMotion: m("(prefers-reduced-motion: reduce)"),
    reducedTransparency: m("(prefers-reduced-transparency: reduce)"),
  };
}

export function resolvePrefs(prefs: ThemePrefs, env: SystemEnv, forcedDark = false): Resolved {
  const light = prefs.appearance === "light" || (prefs.appearance === "system" && env.light);
  return {
    theme: prefs.theme,
    appearance: forcedDark || !light ? "dark" : "light",
    contrast: prefs.contrast === "more" || (prefs.contrast === "system" && env.moreContrast) ? "more" : "standard",
    motion: prefs.motion === "reduced" || env.reducedMotion ? "reduced" : "full",
    transparency: prefs.transparency === "reduced" || env.reducedTransparency ? "reduced" : "full",
  };
}

// ---- state -------------------------------------------------------------------------------

const prefsRef = ref<UserPrefs>({ ...DEFAULT_PREFS });
const resolvedRef = ref<Resolved>(resolvePrefs(DEFAULT_PREFS, readSystemEnv(() => ({ matches: false }))));
let forcedDark = false;

/** Writes the resolved choice to <html>, then the theme-color meta and the tab icon. */
export function applyPrefs(prefs: ThemePrefs): Resolved {
  const r = resolvePrefs(prefs, readSystemEnv(), forcedDark);
  resolvedRef.value = r;
  if (typeof document === "undefined") return r;
  const root = document.documentElement;
  root.dataset.theme = r.theme;
  root.dataset.appearance = r.appearance;
  root.dataset.contrast = r.contrast;
  root.dataset.motion = r.motion;
  root.dataset.transparency = r.transparency;

  const canvas = getComputedStyle(root).getPropertyValue("--cha-canvas").trim();
  const meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (meta && canvas) meta.content = canvas;
  const icon = THEMES.find((t) => t.id === r.theme)?.favicon;
  const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
  if (link && icon && link.getAttribute("href") !== icon) link.setAttribute("href", icon);
  return r;
}

const anySystem = (p: ThemePrefs) =>
  p.appearance === "system" || p.contrast === "system" || p.motion === "system" || p.transparency === "system";

let listening = false;

/** Call once before mounting: loads the saved choice, applies it and follows the OS. */
export function initTheme(): void {
  prefsRef.value = loadPrefs();
  applyPrefs(prefsRef.value);
  if (listening || typeof matchMedia === "undefined") return;
  listening = true;
  for (const q of [
    "(prefers-color-scheme: light)",
    "(prefers-contrast: more)",
    "(prefers-reduced-motion: reduce)",
    "(prefers-reduced-transparency: reduce)",
  ]) {
    matchMedia(q).addEventListener?.("change", () => {
      if (anySystem(prefsRef.value)) applyPrefs(prefsRef.value);
    });
  }
}

/** The stream view stays dark and theme-neutral whatever the user picked. */
export function setForcedDark(on: boolean): void {
  forcedDark = on;
  applyPrefs(prefsRef.value);
}

/** Called after the user changes a preference (not when the server's copy is adopted). */
type ChangeListener = (prefs: UserPrefs) => void;
const changeListeners = new Set<ChangeListener>();

export function onPrefsChange(fn: ChangeListener): () => void {
  changeListeners.add(fn);
  return () => changeListeners.delete(fn);
}

/** Applies and caches prefs without telling the sync (the server's copy, say). */
export function replacePrefs(prefs: UserPrefs): void {
  prefsRef.value = prefs;
  savePrefs(prefs);
  applyPrefs(prefs);
}

export function currentPrefs(): UserPrefs {
  return { ...prefsRef.value, pinned: [...prefsRef.value.pinned] };
}

/** The user's change: applied and cached at once; the sync sends it to the server. */
export function setPrefs(partial: Partial<UserPrefs>): void {
  replacePrefs(parsePrefs({ ...prefsRef.value, ...partial }));
  for (const fn of changeListeners) fn(currentPrefs());
}

export function useTheme() {
  return {
    prefs: readonly(prefsRef),
    resolved: readonly(resolvedRef),
    isDark: computed(() => resolvedRef.value.appearance === "dark"),
    setPrefs,
  };
}
