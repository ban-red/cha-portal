// Pure helpers for custom environments (ADR 0021): the editor's overrides, variables, slug and
// security checks, and how the dashboard words what they cause.

import {
  ApiError,
  type CatalogSecurity,
  type CustomOverrides,
  type CustomTemplate,
  type Environment,
  type HostPort,
  type Template,
} from "./api";
import { approvalGrant } from "./catalogs";
import { isEmptyHost } from "./hostOptions";

export const CUSTOM_KEY = ["admin-custom-templates"] as const;
export const HOST_OPTIONS_KEY = ["admin-host-options"] as const;

export const MAX_ENV_VARS = 64;
export const MAX_ENV_BYTES = 16 * 1024;
/** Set by the node: a custom environment may not (along with anything starting `CHA_`). */
export const RESERVED_ENV = ["HOME", "USER", "XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "PULSE_SERVER", "DISPLAY"] as const;

export const isCustomId = (id: string) => id.startsWith("custom.");
export const customId = (slug: string) => `custom.${slug}`;
export const slugOf = (id: string) => id.replace(/^custom\./, "");

// ---- Slug ---------------------------------------------------------------------------------

const SLUG = /^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$/;

/** Why a slug is refused, or null. */
export function slugProblem(slug: string): string | null {
  if (!slug) return "Give it a short name for its id.";
  if (slug === "migrated" || slug === "migrating") return `"${slug}" is reserved.`;
  if (!SLUG.test(slug)) return "Use lowercase letters, digits and hyphens, up to 40, starting and ending with a letter or digit.";
  return null;
}

/** A slug to offer for a name: "Steam (big picture)" becomes "steam-big-picture". */
export function suggestSlug(name: string): string {
  const s = name
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40)
    .replace(/-+$/g, "");
  return s || "custom";
}

// ---- Variables ----------------------------------------------------------------------------

/** Why a variable name is refused, or null (mirrors `check_env` in `cha-wire`). */
export function envNameProblem(name: string): string | null {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || name.length > 128)
    return `${JSON.stringify(name)} isn't a variable name (letters, digits and _, not starting with a digit)`;
  if (name.startsWith("CHA_") || (RESERVED_ENV as readonly string[]).includes(name)) return `${name} is set by the node`;
  return null;
}

/** The first problem with a set of variables, or null. */
export function checkEnv(env: Record<string, string>): string | null {
  const entries = Object.entries(env);
  if (entries.length > MAX_ENV_VARS) return `At most ${MAX_ENV_VARS} variables`;
  const enc = new TextEncoder();
  const bytes = entries.reduce((n, [k, v]) => n + enc.encode(k).length + enc.encode(v).length + 1, 0);
  if (bytes > MAX_ENV_BYTES) return `At most ${MAX_ENV_BYTES / 1024} KiB of variables`;
  for (const [k, v] of entries) {
    const p = envNameProblem(k);
    if (p) return p;
    if (v.includes("\0")) return `${k} holds a NUL byte`;
  }
  return null;
}

export interface EnvRow {
  key: string;
  value: string;
}

export const envToRows = (env: Record<string, string> | undefined): EnvRow[] =>
  Object.entries(env ?? {}).map(([key, value]) => ({ key, value }));

/** Rows to a map: blank-named rows are dropped. */
export function rowsToEnv(rows: EnvRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) if (r.key.trim()) out[r.key.trim()] = r.value;
  return out;
}

/** A problem per row index (a bad name, or a name listed twice); rows with none are absent. */
export function envRowProblems(rows: EnvRow[]): Record<number, string> {
  const out: Record<number, string> = {};
  const seen = new Set<string>();
  rows.forEach((r, i) => {
    const key = r.key.trim();
    if (!key) {
      if (r.value) out[i] = "Give the variable a name.";
      return;
    }
    const p = envNameProblem(key);
    if (p) out[i] = p;
    else if (seen.has(key)) out[i] = `${key} is listed twice.`;
    else if (r.value.includes("\0")) out[i] = `${key} holds a NUL byte.`;
    seen.add(key);
  });
  return out;
}

// ---- Overrides ----------------------------------------------------------------------------

export type OverrideKey = keyof CustomOverrides;

export const isOverridden = (o: CustomOverrides, key: OverrideKey) => o[key] !== undefined;

/** Overrides with one field set; the object is not changed. */
export function setOverride<K extends OverrideKey>(o: CustomOverrides, key: K, value: CustomOverrides[K]): CustomOverrides {
  return { ...o, [key]: value };
}

/** Overrides with one field back to the base's. */
export function resetOverride(o: CustomOverrides, key: OverrideKey): CustomOverrides {
  const { [key]: _gone, ...rest } = o;
  return rest;
}

/** What to send: blank text overrides, and an empty variable map, are the base's values again. */
export function cleanOverrides(o: CustomOverrides): CustomOverrides {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(o)) {
    if (v === undefined || v === null) continue;
    if (typeof v === "string") {
      const t = v.trim();
      // A description may be emptied on purpose.
      if (!t && k !== "description") continue;
      out[k] = k === "description" ? v : t;
    } else if (k === "env") {
      if (Object.keys(v as object).length) out[k] = v;
    } else {
      out[k] = v;
    }
  }
  return out as CustomOverrides;
}

export const overrideCount = (o: CustomOverrides) => Object.keys(cleanOverrides(o)).length;

/** The template as it resolves: the base with the overrides on top (what the server does at launch; for previews). */
export function resolve(base: Template, o: CustomOverrides): Template {
  return {
    ...base,
    ...(o.name !== undefined && { name: o.name }),
    ...(o.description !== undefined && { description: o.description }),
    ...(o.image !== undefined && { image: o.image }),
    ...(o.class !== undefined && { class: o.class }),
    ...(o.shmMb !== undefined && { shmMb: o.shmMb }),
    ...(o.fixedSize !== undefined && { fixedSize: o.fixedSize }),
    ...(o.needsGpu !== undefined && { needsGpu: o.needsGpu }),
    ...(o.security !== undefined && { security: o.security }),
  };
}

// ---- Security -----------------------------------------------------------------------------

// vm is last, as on the server (custom.rs): not a wider sandbox, but a device
// none of the others get.
const RANK: Record<CatalogSecurity, number> = { standard: 0, browser: 1, steam: 2, vm: 3 };

/** The profile grants more than the base's. */
export const widerSecurity = (base: CatalogSecurity, chosen: CatalogSecurity) => RANK[chosen] > RANK[base];

/** The note to show beside a profile wider than the base's, or null. */
export function securityNote(base: CatalogSecurity, chosen: CatalogSecurity): string | null {
  if (!widerSecurity(base, chosen)) return null;
  const grant = approvalGrant(chosen);
  return `Wider than ${base}, the base's profile: it ${grant ?? "grants more"}. Saving it is recorded in the audit log like an approval.`;
}

// ---- Duplicate ----------------------------------------------------------------------------

export const SHARE_DATA_EXPLAIN =
  "Two environments on the same data can't run at once: launching one while the other is live is refused.";

/** The editor's address for a new custom environment. */
export function newCustomQuery(base: string, slug: string, shareData: boolean) {
  return { base, slug, shareData: shareData ? "1" : "0" };
}

export const parseShareData = (v: unknown) => v === "1" || v === "true";

// ---- Dashboard ----------------------------------------------------------------------------

/** "host:27016/udp" lines for a running environment's ports (the node's address, else its name). */
export function portLines(e: Pick<Environment, "streamer" | "nodeName"> & { ports?: HostPort[] | null }): string[] {
  const where = e.streamer?.host ?? e.nodeName ?? "the node";
  return (e.ports ?? []).map((p) => `${where}:${p.host ?? p.container}/${p.protocol}`);
}

/** "uses Steam's data" for a custom template that shares its base's. */
export function dataNote(t: Pick<Template, "custom">, baseName: string): string | null {
  return t.custom?.shareData ? `uses ${baseName}'s data` : null;
}

/** The words for a failed launch; a clash on shared data says what to do. */
export function launchErrorText(err: unknown, fallback: string): string {
  if (!(err instanceof ApiError)) return fallback;
  if (err.code === "data_in_use") {
    const why = err.message ? `${err.message.replace(/\.$/, "")}. ` : "";
    return `Its saved data is in use by another environment of yours. ${why}Stop that one first: two environments can't run on the same data.`;
  }
  return err.message || fallback;
}

/** One line about a custom environment, for its list row. */
export function summaryLine(c: Pick<CustomTemplate, "baseName" | "shareData" | "overrides" | "host">): string {
  const n = overrideCount(c.overrides);
  const bits = [`Based on ${c.baseName}`, n ? `${n} ${n === 1 ? "change" : "changes"}` : "no changes yet"];
  bits.push(c.shareData ? "shares its saved data" : "own saved data");
  if (!isEmptyHost(c.host)) bits.push("host options");
  return bits.join(" · ");
}
