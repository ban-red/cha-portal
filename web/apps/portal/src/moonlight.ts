// Moonlight hosts on the dashboard: which of a host's apps show, and whether each can launch.

import type { AdoptedHost, Environment, MoonlightApp } from "./api";

export const MOONLIGHT_HOSTS_KEY = ["moonlight", "hosts"] as const;
export const MOONLIGHT_FOUND_KEY = ["moonlight", "found"] as const;

/** The latest live environment of each template id (what a card opens instead of launching again). */
export function liveByTemplate(envs: readonly Environment[]): Map<string, Environment> {
  const m = new Map<string, Environment>();
  for (const e of envs) {
    if (e.state === "destroyed" || e.state === "failed") continue;
    const have = m.get(e.templateId);
    if (!have || e.createdAt > have.createdAt) m.set(e.templateId, e);
  }
  return m;
}

/** A host's apps matching the search (`fold` normalises case and accents), sorted. */
export function visibleApps(
  host: Pick<AdoptedHost, "apps">,
  query: string,
  fold: (s: string) => string,
  sort: "name" | "recent",
  lastUsed: ReadonlyMap<string, number>,
): MoonlightApp[] {
  const q = fold(query.trim());
  return host.apps
    .filter((a) => !q || fold(a.name).includes(q))
    .sort((a, b) => {
      if (sort === "recent") {
        const recent = (lastUsed.get(b.templateId) ?? 0) - (lastUsed.get(a.templateId) ?? 0);
        if (recent) return recent;
      }
      return a.name.localeCompare(b.name);
    });
}

/** Whether a host's search/filter leaves its section worth showing: it matches by its own name too. */
export function hostMatches(
  host: Pick<AdoptedHost, "name">,
  query: string,
  fold: (s: string) => string,
): boolean {
  const q = fold(query.trim());
  return !q || fold(host.name).includes(q);
}

export interface LaunchState {
  /** The user's own environment on this app: Connect/Open instead of Launch. */
  instance?: Environment;
  /** Launch can't be pressed. */
  disabled: boolean;
  /** Why, as text. */
  reason: string | null;
}

/** Can this app be launched now? A running environment of the user's own app is opened instead. */
export function launchState(host: AdoptedHost, app: MoonlightApp, live: ReadonlyMap<string, Environment>, guest = false): LaunchState {
  const instance = live.get(app.templateId);
  if (instance) return { instance, disabled: false, reason: null };
  if (guest) return { disabled: true, reason: null };
  if (!host.online) return { disabled: true, reason: "Host is offline." };
  if (host.busy) return { disabled: true, reason: `In use by ${host.busy.owner}.` };
  return { disabled: false, reason: null };
}

/** Whether an API failure means "this server has no Moonlight support". */
export function isMissing(err: unknown): boolean {
  return typeof err === "object" && err !== null && (err as { status?: number }).status === 404;
}
