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
}

export const THEMES = [
  {
    id: "cha-jade",
    name: "Cha – Jade",
    appearances: ["dark"],
    swatches: ["#0a0f11", "#10181b", "#2dd4bf", "#4ade80"],
  },
] as const satisfies readonly ThemeInfo[];

export type ThemeId = (typeof THEMES)[number]["id"];
