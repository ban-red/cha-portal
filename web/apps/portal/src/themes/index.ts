// The theme registry and the role contract. Each theme is one CSS file in this folder
// (`<id>.css`) that defines every role as a `--cha-<role>` variable; style.css maps the
// roles to Tailwind colours. `themes.test.ts` checks that the files and this list agree
// and that every contrast pair passes.

export const ROLES = [
  "canvas",
  "panel",
  "panel-2",
  "line",
  "line-strong",
  "ink",
  "ink-2",
  "ink-3",
  "accent",
  "accent-fill",
  "accent-fill-hover",
  "on-accent",
  "accent-soft",
  "focus",
  "ok",
  "warn",
  "danger",
  "info",
  "scrim",
  "chart-1",
  "chart-2",
  "chart-3",
  "chart-4",
] as const;

export type Role = (typeof ROLES)[number];
export type Appearance = "dark" | "light";

export interface ThemeInfo {
  id: string;
  name: string;
  appearances: readonly Appearance[];
  /** Preview colours for a theme picker: canvas, panel, accent, ok. */
  swatches: readonly string[];
  /** The tab icon for this theme (a file in public/). */
  favicon: string;
}

export const THEMES = [
  {
    id: "cha-magenta",
    name: "Cha – Magenta",
    appearances: ["dark", "light"],
    swatches: ["#12060f", "#1c0b18", "#ff4fb3", "#34d399"],
    favicon: "/favicon.svg",
  },
  {
    id: "cha-jade",
    name: "Cha – Jade",
    appearances: ["dark", "light"],
    swatches: ["#0a0f11", "#10181b", "#2dd4bf", "#4ade80"],
    favicon: "/favicon-jade.svg",
  },
] as const satisfies readonly ThemeInfo[];

export type ThemeId = (typeof THEMES)[number]["id"];

/** Shown when nothing is stored, and what a bare <html> (no data-theme) renders as. */
export const DEFAULT_THEME: ThemeId = "cha-magenta";

/** What the user picked. "system" follows the OS setting. Resolution is in runtime.ts. */
export type ThemePrefs = {
  theme: ThemeId;
  appearance: "system" | "dark" | "light";
  contrast: "system" | "standard" | "more";
  motion: "system" | "reduced";
  transparency: "system" | "reduced";
};

/** The Environments page's view, sort and pins: kept per user beside the theme. */
export type EnvPrefs = {
  /** Template ids the user pinned (unique, at most MAX_PINNED). */
  pinned: string[];
  envView: "grid" | "list";
  envSort: "name" | "recent";
};

/** Everything saved per user (`GET`/`PUT /api/me/prefs`): the theme choices and the page's. */
export type UserPrefs = ThemePrefs & EnvPrefs;

export const MAX_PINNED = 64;

export const DEFAULT_PREFS: UserPrefs = {
  theme: DEFAULT_THEME,
  appearance: "system",
  contrast: "system",
  motion: "system",
  transparency: "system",
  pinned: [],
  envView: "grid",
  envSort: "name",
};
