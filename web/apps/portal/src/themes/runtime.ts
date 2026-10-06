// Applies the theme preferences to <html>. The inline script in index.html does the same
// resolution before first paint: the two MUST agree (same keys, same "system" rules), so
// change both together.
import { computed, readonly, ref } from "vue";

import { DEFAULT_PREFS, THEMES, type ThemeId, type ThemePrefs } from "./index";

export const STORAGE_KEY = "cha.theme";

const APPEARANCES = ["system", "dark", "light"] as const;
const CONTRASTS = ["system", "standard", "more"] as const;
const MOTIONS = ["system", "reduced"] as const;
const TRANSPARENCIES = ["system", "reduced"] as const;

function pick<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === "string" && (allowed as readonly string[]).includes(v) ? (v as T) : fallback;
}

/** Any value to valid prefs, field by field: unknown or invalid values take the default. */
export function parsePrefs(raw: unknown): ThemePrefs {
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
  };
}

export function loadPrefs(): ThemePrefs {
  try {
    const s = localStorage.getItem(STORAGE_KEY);
    return parsePrefs(s ? JSON.parse(s) : null);
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

export function savePrefs(prefs: ThemePrefs): void {
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

const prefsRef = ref<ThemePrefs>({ ...DEFAULT_PREFS });
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

export function setPrefs(partial: Partial<ThemePrefs>): void {
  prefsRef.value = parsePrefs({ ...prefsRef.value, ...partial });
  savePrefs(prefsRef.value);
  applyPrefs(prefsRef.value);
}

export function useTheme() {
  return {
    prefs: readonly(prefsRef),
    resolved: readonly(resolvedRef),
    isDark: computed(() => resolvedRef.value.appearance === "dark"),
    setPrefs,
  };
}
