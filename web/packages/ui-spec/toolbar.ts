// The toolbar's spec (toolbar.json, ADR 0016) and its model: `buildToolbar` turns the session's state
// and the capabilities it reports into the controls, menus and countdown both players draw.
// crates/cha-ui-spec/src/toolbar.rs is the Rust twin; toolbar-cases.json keeps them equal.
import toolbarJson from "./toolbar.json";
import { fillPanel, STATS_PANEL, type PanelValues, type Tone } from "./panel";
import type { Platform, PlatformText } from "./index";

/** What a colour means; the renderer maps it to a theme colour. `faint` is the tertiary ink, `dim` the secondary. */
export type ToolbarTone = "none" | "ok" | "accent" | "warn" | "danger" | "dim" | "faint";

/** A condition on the state: `key`, `!key`, `key=value`, `key!=value` or `key>number` (see the spec's notes). */
export type ToolbarCond = string;

/** A state value. A list holds objects (`controllers`, `watchers`) or strings (`codecs`). */
export type StateValue = boolean | number | string | null | undefined | readonly unknown[];
export type ToolbarState = Record<string, StateValue>;

/** The fields a variant may override. */
export interface ToolbarVariantFields {
  icon?: string;
  label?: PlatformText;
  aria_label?: PlatformText;
  tooltip?: PlatformText;
  tone?: ToolbarTone;
  disabled?: boolean;
  read_only?: PlatformText;
  aria_text?: PlatformText;
}

export interface ToolbarVariant extends ToolbarVariantFields {
  when?: ToolbarCond[];
  platforms?: Platform[];
}

export interface ToolbarBadgeSpec {
  when?: ToolbarCond[];
  text: string;
  /** A fixed tone, or ... */
  tone?: ToolbarTone;
  /** ... the stats panel's tone for the grade letter in the state's `grade`. */
  tone_from?: "grade";
  tooltip?: PlatformText;
}

export interface ToolbarOptionSpec extends ToolbarVariantFields {
  value: string;
  label: PlatformText;
  platforms?: Platform[];
  when?: ToolbarCond[];
  states?: ToolbarVariant[];
}

export type ToolbarRowKind = "choice" | "button" | "slider" | "list" | "note" | "link";

export interface ToolbarRowSpec extends ToolbarVariantFields {
  id: string;
  kind: ToolbarRowKind;
  platforms?: Platform[];
  needs?: string[];
  lacks?: string[];
  when?: ToolbarCond[];
  states?: ToolbarVariant[];
  group?: string;
  dom_id?: string;
  role?: "status";
  /** A note's text. */
  text?: PlatformText;
  /** A choice: the state key holding the current value. A slider: the key holding the level. */
  value?: string;
  options?: { items: ToolbarOptionSpec[] } | { from: "codecs" };
  /** A choice is changeable only when every one of these is reported; otherwise it reads `read_only` (or is left out). */
  edit_needs?: string[];
  zero_when?: ToolbarCond[];
  min?: number;
  max?: number;
  step?: number;
  display?: string;
  /** A list row: the state key holding the entries. */
  from?: string;
  item_detail?: string;
  item_detail_none?: string;
  to?: string;
}

export interface ToolbarMenuSpec {
  id: string;
  dom_id: string;
  align: "left" | "right";
  rows: ToolbarRowSpec[];
}

export type ToolbarControlKind = "button" | "toggle" | "menu" | "label" | "badge" | "list";

export interface ToolbarControlSpec extends ToolbarVariantFields {
  id: string;
  kind: ToolbarControlKind;
  group: "left" | "right";
  platforms?: Platform[];
  needs?: string[];
  when?: ToolbarCond[];
  droppable?: boolean;
  hover_tone?: ToolbarTone;
  pressed?: ToolbarCond[];
  states?: ToolbarVariant[];
  badge?: ToolbarBadgeSpec;
  menu?: ToolbarMenuSpec;
  from?: string;
  item_action?: { label: string; tooltip: string };
}

export interface ToolbarStateSpec {
  kind: "bool" | "number" | "string" | "enum" | "list";
  platforms?: Platform[];
  values?: string[];
  derived?: boolean;
  doc: string;
}

export interface ToolbarSpec {
  notes: string;
  timing: { fold_after_ms: number; near_top_px: number };
  power_off: {
    seconds: number;
    default_name: string;
    title: string;
    stopping: string;
    failed: string;
    note: string;
    cancel: { label: string; tooltip: PlatformText };
    close: string;
  };
  folded: {
    bar: { aria_label: string; tooltip: string };
    tab: { aria_label: string; tooltip: string; disabled: ToolbarCond[] };
  };
  visibility: { hide: { platforms?: Platform[]; when: ToolbarCond[] }[]; show: { platforms?: Platform[]; when: ToolbarCond[] }[] };
  capabilities: Record<string, { doc: string }>;
  reports: Record<Platform, string[]>;
  state: Record<string, ToolbarStateSpec>;
  codec_labels: Record<string, string>;
  controls: ToolbarControlSpec[];
}

export const TOOLBAR = toolbarJson as unknown as ToolbarSpec;

// --- the model ---

export interface ToolbarBadge {
  text: string;
  tone: ToolbarTone;
  tooltip: string | null;
}

export interface ToolbarOption {
  value: string;
  label: string;
  disabled: boolean;
}

export interface ToolbarItem {
  /** The entry's words: a controller's name, a watcher's label. */
  text: string;
  /** A controller's slot ("slot 2", "no slot"). */
  detail: string | null;
  /** A watcher's id. */
  id: string | null;
  /** The button beside the entry, when it has one. */
  action: { label: string; tooltip: string } | null;
}

export interface ToolbarRow {
  id: string;
  kind: ToolbarRowKind;
  group: string | null;
  dom_id: string | null;
  role: "status" | null;
  label: string | null;
  tooltip: string | null;
  /** A note's, link's or button's words. */
  text: string | null;
  tone: ToolbarTone;
  disabled: boolean;
  // choice
  value: string | null;
  options: ToolbarOption[] | null;
  editable: boolean;
  read_only: string | null;
  // slider
  slider: { min: number; max: number; step: number; value: number; display: string; aria_text: string } | null;
  // list
  items: ToolbarItem[] | null;
  // link
  to: string | null;
}

export interface ToolbarMenu {
  id: string;
  dom_id: string;
  align: "left" | "right";
  rows: ToolbarRow[];
}

export interface ToolbarControl {
  id: string;
  kind: ToolbarControlKind;
  group: "left" | "right";
  droppable: boolean;
  icon: string | null;
  label: string | null;
  aria_label: string | null;
  tooltip: string | null;
  /** A toggle's state (`aria-pressed`); null for the rest. */
  pressed: boolean | null;
  /** A menu's button while its menu is open. */
  active: boolean;
  disabled: boolean;
  tone: ToolbarTone;
  hover_tone: ToolbarTone;
  badge: ToolbarBadge | null;
  /** The open menu, resolved; null while closed. */
  menu: ToolbarMenu | null;
  /** A menu button's menu id, open or not. */
  menu_dom_id: string | null;
  /** A list control's entries. */
  items: ToolbarItem[] | null;
}

export interface ToolbarCountdown {
  seconds: number;
  /** seconds / total, 1 at the start. */
  fraction: number;
  title: string;
  note: string;
  cancel_label: string;
  cancel_tooltip: string | null;
}

export interface ToolbarModel {
  /** The bar is up (otherwise only its folded bar shows). */
  visible: boolean;
  controls: ToolbarControl[];
  countdown: ToolbarCountdown | null;
  folded_bar: { aria_label: string; tooltip: string };
  hide_tab: { aria_label: string; tooltip: string; disabled: boolean };
}

// --- conditions ---

function present(v: StateValue): boolean {
  if (v === undefined || v === null || v === false || v === "") return false;
  if (typeof v === "number") return Number.isFinite(v);
  if (Array.isArray(v)) return v.length > 0;
  return true;
}

/** A value as `key=value` compares it: a number without a fraction reads as an integer. */
function plain(v: StateValue): string {
  if (v === undefined || v === null) return "";
  return typeof v === "string" ? v : String(v);
}

function holds(cond: ToolbarCond, state: ToolbarState): boolean {
  const m = /^([a-z][a-z0-9_]*)(!=|=|>)(.*)$/.exec(cond);
  if (m) {
    const v = state[m[1]!];
    if (m[2] === "=") return present(v) && plain(v) === m[3];
    if (m[2] === "!=") return plain(v) !== m[3];
    return typeof v === "number" && v > Number(m[3]);
  }
  return cond.startsWith("!") ? !present(state[cond.slice(1)]) : present(state[cond]);
}

const all = (conds: ToolbarCond[] | undefined, state: ToolbarState): boolean => (conds ?? []).every((c) => holds(c, state));
const onPlatform = (platforms: Platform[] | undefined, platform: Platform) => !platforms || platforms.includes(platform);

interface Ctx {
  spec: ToolbarSpec;
  state: ToolbarState;
  caps: ReadonlySet<string>;
  platform: Platform;
  values: PanelValues;
}

const shown = (c: { platforms?: Platform[]; needs?: string[]; lacks?: string[]; when?: ToolbarCond[] }, x: Ctx): boolean =>
  onPlatform(c.platforms, x.platform) &&
  (c.needs ?? []).every((n) => x.caps.has(n)) &&
  !(c.lacks ?? []).some((n) => x.caps.has(n)) &&
  all(c.when, x.state);

/** The variants that apply, in order, laid over the base. */
function resolve<T extends ToolbarVariantFields>(base: T, states: ToolbarVariant[] | undefined, x: Ctx): T {
  const out: T = { ...base };
  for (const s of states ?? []) {
    if (!onPlatform(s.platforms, x.platform) || !all(s.when, x.state)) continue;
    const { when: _when, platforms: _platforms, ...fields } = s;
    Object.assign(out, fields);
  }
  return out;
}

/** The text for the platform, filled; null when the spec has none for it. */
function words(t: PlatformText | undefined, x: Ctx, extra: PanelValues = {}): string | null {
  if (t === undefined) return null;
  const s = typeof t === "string" ? t : t[x.platform];
  return s === undefined ? null : fillPanel(s, { ...x.values, ...extra });
}

// --- building ---

const GRADE_TONE: Record<string, Tone> = STATS_PANEL.grade_tone;

function toolTone(t: Tone): ToolbarTone {
  return t;
}

function items(list: readonly unknown[], spec: { item_action?: { label: string; tooltip: string }; item_detail?: string; item_detail_none?: string }, x: Ctx): ToolbarItem[] {
  return list.map((e) => {
    if (typeof e !== "object" || e === null) return { text: String(e), detail: null, id: null, action: null };
    const o = e as { id?: string; label?: string; name?: string; slot?: number | null; can_hand?: boolean };
    const detail =
      spec.item_detail === undefined
        ? null
        : o.slot === null || o.slot === undefined
          ? (spec.item_detail_none ?? null)
          : fillPanel(spec.item_detail, { ...x.values, n: o.slot + 1 });
    return {
      text: o.label ?? o.name ?? "",
      detail,
      id: o.id ?? null,
      action: spec.item_action && o.can_hand ? spec.item_action : null,
    };
  });
}

function options(r: ToolbarRowSpec, x: Ctx): ToolbarOption[] {
  const o = r.options;
  if (!o) return [];
  if ("from" in o) {
    const list = x.state.codecs;
    return (Array.isArray(list) ? list : []).map((c) => ({ value: String(c), label: x.spec.codec_labels[String(c)] ?? String(c).toUpperCase(), disabled: false }));
  }
  const out: ToolbarOption[] = [];
  for (const item of o.items) {
    if (!onPlatform(item.platforms, x.platform) || !all(item.when, x.state)) continue;
    const merged = resolve(item, item.states, x);
    out.push({ value: item.value, label: words(merged.label, x) ?? "", disabled: merged.disabled === true });
  }
  return out;
}

function row(r: ToolbarRowSpec, x: Ctx): ToolbarRow | null {
  if (!shown(r, x)) return null;
  const m = resolve(r, r.states, x);
  const editable = r.kind === "choice" && (r.edit_needs ?? []).every((n) => x.caps.has(n));
  const readOnly = words(m.read_only, x);
  if (r.kind === "choice" && !editable && readOnly === null) return null;
  const out: ToolbarRow = {
    id: r.id,
    kind: r.kind,
    group: r.group ?? null,
    dom_id: r.dom_id ?? null,
    role: r.role ?? null,
    label: r.kind === "note" ? null : words(m.label, x),
    tooltip: words(m.tooltip, x),
    text: words(r.text, x),
    tone: m.tone ?? "none",
    disabled: m.disabled === true,
    value: null,
    options: null,
    editable: false,
    read_only: null,
    slider: null,
    items: null,
    to: r.to ?? null,
  };
  if (r.kind === "choice") {
    const v = x.state[r.value!];
    out.value = present(v) ? plain(v) : null;
    out.options = options(r, x);
    out.editable = editable;
    out.read_only = readOnly;
  } else if (r.kind === "slider") {
    const raw = x.state[r.value!];
    const level = all(r.zero_when, x.state) ? 0 : typeof raw === "number" ? raw : 0;
    const shownLevel = { shown: level };
    out.slider = {
      min: r.min!,
      max: r.max!,
      step: r.step!,
      value: level,
      display: words(r.display!, x, shownLevel) ?? "",
      aria_text: words(m.aria_text, x, shownLevel) ?? "",
    };
  } else if (r.kind === "list") {
    const list = x.state[r.from!];
    out.items = items(Array.isArray(list) ? list : [], r, x);
  }
  return out;
}

function control(c: ToolbarControlSpec, x: Ctx): ToolbarControl | null {
  if (!shown(c, x)) return null;
  const m = resolve(c, c.states, x);
  const active = c.menu !== undefined && plain(x.state.menu_open) === c.menu.id;
  let badge: ToolbarBadge | null = null;
  if (c.badge && all(c.badge.when, x.state)) {
    const grade = plain(x.state.grade);
    badge = {
      text: fillPanel(c.badge.text, x.values),
      tone: c.badge.tone_from === "grade" ? toolTone(GRADE_TONE[grade] ?? GRADE_TONE.none!) : (c.badge.tone ?? "none"),
      tooltip: words(c.badge.tooltip, x),
    };
  }
  let menu: ToolbarMenu | null = null;
  if (c.menu && active) {
    const rows: ToolbarRow[] = [];
    for (const r of c.menu.rows) {
      const built = row(r, x);
      if (built) rows.push(built);
    }
    menu = { id: c.menu.id, dom_id: c.menu.dom_id, align: c.menu.align, rows };
  }
  let list: ToolbarItem[] | null = null;
  if (c.kind === "list") {
    const entries = x.state[c.from!];
    list = items(Array.isArray(entries) ? entries : [], c, x);
  }
  return {
    id: c.id,
    kind: c.kind,
    group: c.group,
    droppable: c.droppable === true,
    icon: m.icon ?? null,
    label: words(m.label, x),
    aria_label: words(m.aria_label, x),
    tooltip: words(m.tooltip, x),
    pressed: c.kind === "toggle" ? all(c.pressed, x.state) : null,
    active,
    disabled: m.disabled === true,
    tone: m.tone ?? "none",
    hover_tone: c.hover_tone ?? "none",
    badge,
    menu,
    menu_dom_id: c.menu?.dom_id ?? null,
    items: list,
  };
}

/** The toolbar a player draws: the controls it shows now, with their icon, words and state resolved. Pure, so both players agree. */
export function buildToolbar(spec: ToolbarSpec, state: ToolbarState, capabilities: readonly string[], platform: Platform): ToolbarModel {
  const controllers = state.controllers;
  const values: PanelValues = {};
  for (const [k, v] of Object.entries(state)) {
    if (typeof v === "string" || typeof v === "number") values[k] = v;
  }
  values.controllers_count = Array.isArray(controllers) ? controllers.length : 0;
  values.power_off_seconds = spec.power_off.seconds;
  const x: Ctx = { spec, state, caps: new Set(capabilities), platform, values };

  const hidden = spec.visibility.hide.some((v) => onPlatform(v.platforms, platform) && all(v.when, state));
  const visible = !hidden && spec.visibility.show.some((v) => onPlatform(v.platforms, platform) && all(v.when, state));

  const controls: ToolbarControl[] = [];
  for (const c of spec.controls) {
    const built = control(c, x);
    if (built) controls.push(built);
  }

  let countdown: ToolbarCountdown | null = null;
  const left = state.countdown;
  if (typeof left === "number" && Number.isFinite(left)) {
    const p = spec.power_off;
    const name = plain(state.title) || p.default_name;
    const v = { name, seconds: left };
    countdown = {
      seconds: left,
      fraction: Math.min(1, Math.max(0, left / p.seconds)),
      title: fillPanel(p.title, v),
      note: p.note,
      cancel_label: p.cancel.label,
      cancel_tooltip: words(p.cancel.tooltip, x),
    };
  }

  return {
    visible,
    controls,
    countdown,
    folded_bar: spec.folded.bar,
    hide_tab: { aria_label: spec.folded.tab.aria_label, tooltip: spec.folded.tab.tooltip, disabled: all(spec.folded.tab.disabled, state) },
  };
}
