// Shared bits of the app-data views: query keys, cache patching and copy.

import type { QueryClient } from "@tanstack/vue-query";

import { ApiError, type SharedAccess } from "./api";

export const STORAGE_KEY = ["storage"] as const;
export const ADMIN_STORAGE_KEY = ["admin-storage"] as const;

/** The server doesn't have app-data settings yet (it answers 404 to unknown API paths). */
export function notAvailable(err: unknown): boolean {
  return err instanceof ApiError && err.status === 404;
}

/** The server's reason when it sent one; HTTP status text doesn't count. */
export function serverMessage(err: unknown, fallback: string): string {
  return err instanceof ApiError && err.message ? err.message : fallback;
}

/** Replaces one app's entry in a cached `{ apps }` list, leaving the rest alone. */
export function patchApp<T extends { template: string }>(
  queryClient: QueryClient,
  key: readonly unknown[],
  template: string,
  patch: Partial<T>,
): void {
  queryClient.setQueryData<{ apps: T[] }>(key, (old) =>
    old ? { ...old, apps: old.apps.map((a) => (a.template === template ? { ...a, ...patch } : a)) } : old,
  );
}

/** Where an app's shared data is: the node's own place for it, or under the data root. */
export function sharedPath(root: string, app: { template: string; sharedPath?: string }): string {
  return app.sharedPath ?? `${root.replace(/\/+$/, "")}/shared/${app.template}`;
}

/** Where an app's data lives for the user, without the user's id. */
export function dataPath(root: string, template: string): string {
  return `${root.replace(/\/+$/, "")}/users/…/${template}`;
}

export const SHARED_OPTIONS: { value: SharedAccess; label: string }[] = [
  { value: "none", label: "None" },
  { value: "read", label: "Read only" },
  { value: "write", label: "Read and write" },
];

/** Steam's shared data is a game library; for other apps it is just data. */
export function sharedNote(template: string, access: SharedAccess): string {
  const what = template === "steam" ? "a library" : "data";
  return `Shares ${what} with other users (${access === "read" ? "read-only" : "read and write"})`;
}
