// The shared toolbar cases (toolbar-cases.json), typed, and the helper both of this package's runners
// use. Kept out of `index.ts` so the portal's bundle doesn't pull the cases in.
import casesJson from "./toolbar-cases.json";
import type { ToolbarModel, ToolbarState } from "./toolbar";
import type { Platform } from "./index";

/** What a case pins: whether the bar is up, exactly which controls show, some of their fields, the open menu's rows, the countdown. */
export interface ExpectedToolbar {
  visible: boolean;
  /** Exactly the controls shown, in order. */
  controls: string[];
  /** For the controls named, the fields given (every field a control has can be pinned: icon, label, aria_label, tooltip, pressed, active, disabled, tone, hover_tone, badge, items). */
  pin?: Record<string, Record<string, unknown>>;
  /** The open menu: the control it belongs to, exactly its rows in order, and some rows' fields. */
  menu?: { control: string; rows: string[]; pin?: Record<string, Record<string, unknown>> };
  /** The countdown's fields, or null for none. Left out: not checked. */
  countdown?: Record<string, unknown> | null;
  folded_bar?: Record<string, unknown>;
  hide_tab?: Record<string, unknown>;
}

export interface ToolbarCase {
  name: string;
  /** Absent means both. */
  platforms?: Platform[];
  /** A name from the file's `caps`, or the capabilities themselves. */
  caps: string | string[];
  /** Laid over the file's `defaults.state` and the platform's defaults. */
  state: ToolbarState;
  /** What both players show, or what each does. */
  expect: ExpectedToolbar | { web?: ExpectedToolbar; native?: ExpectedToolbar };
}

const file = casesJson as unknown as {
  caps: Record<string, string[]>;
  defaults: { state: ToolbarState; web: ToolbarState; native: ToolbarState };
  cases: ToolbarCase[];
};

export const TOOLBAR_CASES = file.cases;
export const TOOLBAR_CAPS = file.caps;
/** What a session holds with nothing special going on, written out in the file and not read from the spec. */
export const TOOLBAR_DEFAULTS = file.defaults;

export const runsOn = (c: { platforms?: Platform[] }, platform: Platform): boolean => !c.platforms || c.platforms.includes(platform);

export const capsOf = (c: ToolbarCase): string[] => (typeof c.caps === "string" ? file.caps[c.caps]! : c.caps);
/** The state a case gives a platform: the shared defaults, that platform's own, then the case's. */
export const stateOf = (c: ToolbarCase, platform: Platform): ToolbarState => ({ ...file.defaults.state, ...file.defaults[platform], ...c.state });

/** What a case expects on a platform. */
export function expectedFor(c: ToolbarCase, platform: Platform): ExpectedToolbar {
  const e = c.expect as ExpectedToolbar & { web?: ExpectedToolbar; native?: ExpectedToolbar };
  return "visible" in e ? e : (e[platform] as ExpectedToolbar);
}

function pick(from: object, fields: string[]): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const f of fields) out[f] = (from as Record<string, unknown>)[f] ?? null;
  return out;
}

/** The toolbar in the shape a case pins: the same ids, and the same fields of the ones the case lists. */
export function describeToolbar(model: ToolbarModel, want: ExpectedToolbar): ExpectedToolbar {
  const out: ExpectedToolbar = { visible: model.visible, controls: model.controls.map((c) => c.id) };
  if (want.pin) {
    out.pin = {};
    for (const [id, fields] of Object.entries(want.pin)) {
      const c = model.controls.find((x) => x.id === id);
      out.pin[id] = c ? pick(c, Object.keys(fields)) : { missing: true };
    }
  }
  if (want.menu) {
    const owner = model.controls.find((x) => x.id === want.menu!.control);
    const menu = owner?.menu ?? null;
    out.menu = { control: want.menu.control, rows: menu ? menu.rows.map((r) => r.id) : [] };
    if (want.menu.pin) {
      out.menu.pin = {};
      for (const [id, fields] of Object.entries(want.menu.pin)) {
        const r = menu?.rows.find((x) => x.id === id);
        out.menu.pin[id] = r ? pick(r, Object.keys(fields)) : { missing: true };
      }
    }
  }
  if (want.countdown !== undefined) out.countdown = want.countdown === null || !model.countdown ? null : pick(model.countdown, Object.keys(want.countdown));
  if (want.folded_bar) out.folded_bar = pick(model.folded_bar, Object.keys(want.folded_bar));
  if (want.hide_tab) out.hide_tab = pick(model.hide_tab, Object.keys(want.hide_tab));
  return out;
}
