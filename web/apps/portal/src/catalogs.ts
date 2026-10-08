// Pure helpers for the Catalogs admin page (ADR 0019).

import { ApiError, type CatalogSecurity, type CatalogTemplateView, type CatalogView } from "./api";

export const CATALOGS_KEY = ["admin-catalogs"] as const;

/** The guide for catalog authors. The SPA has no repository URL setting, so this is the project's. */
export const IMAGE_SPEC_URL = "https://github.com/ban-red/cha-portal/blob/main/docs/image-spec.md";

/** What an admin grants by approving a template, or null for a profile that needs no approval. */
export function approvalGrant(security: CatalogSecurity): string | null {
  if (security === "browser") return "lets this app's container create user namespaces (browser sandboxes)";
  if (security === "steam")
    return "lets this app's container create user namespaces (browser sandboxes) and run under the cha-sandbox AppArmor profile";
  return null;
}

export const needsApproval = (t: Pick<CatalogTemplateView, "security">) => approvalGrant(t.security) !== null;

/** "From https://…" or "Pasted". */
export const sourceLabel = (c: Pick<CatalogView, "url">) => (c.url ? c.url : "Pasted");

/** The server's own words for a failed call; a fallback when there are none. */
export function errorText(err: unknown, fallback: string): string {
  return err instanceof ApiError && err.message ? err.message : err instanceof Error && err.message ? err.message : fallback;
}

/** "3 of 5 available" for the list's summary line. */
export function availability(c: Pick<CatalogView, "templates">): string {
  const n = c.templates.length;
  const ok = c.templates.filter((t) => t.available).length;
  return `${n} ${n === 1 ? "app" : "apps"}, ${ok} available`;
}

/** An empty or blank slug is left out of the request, so the server takes the document's `id`. */
export function addBody(
  mode: "url" | "paste",
  slug: string,
  value: string,
): { slug?: string } & ({ url: string } | { document: string }) {
  const s = slug.trim();
  const base = s ? { slug: s } : {};
  return mode === "url" ? { ...base, url: value.trim() } : { ...base, document: value };
}
