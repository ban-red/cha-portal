// The classes the session toolbar and its menus share. Written out here so Tailwind sees them.
import type { ToolbarTone } from "@cha/ui-spec";

export const ICON_BTN = "btn-ghost relative grid size-8 place-items-center border-0 p-0";
export const MENU_BOX = "absolute top-full z-20 mt-2 w-64 rounded-xl border border-line bg-panel p-3 text-left text-xs whitespace-normal shadow-lg";
export const SELECT = "w-full rounded-lg border border-line-strong bg-canvas px-2 py-1 text-xs text-ink-2 disabled:opacity-50";

/** The text colour of a spec tone. */
export const TONE_TEXT: Record<ToolbarTone, string> = {
  none: "",
  ok: "text-ok",
  accent: "text-accent",
  warn: "text-warn",
  danger: "text-danger",
  dim: "text-ink-2",
  faint: "text-ink-3",
};

/** The text colour under the pointer, for a button that warns (power off). */
export const HOVER_TEXT: Record<ToolbarTone, string> = {
  none: "",
  ok: "hover:text-ok",
  accent: "hover:text-accent",
  warn: "hover:text-warn",
  danger: "hover:text-danger",
  dim: "hover:text-ink-2",
  faint: "hover:text-ink-3",
};
