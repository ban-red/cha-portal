// Types for the spec's JSON files (ADR 0016). `crates/cha-ui-spec` reads the same files.
import iconsJson from "./icons.json";

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
