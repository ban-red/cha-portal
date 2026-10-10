// Keeps the user's preferences (theme choices, Environments view and pins) in step with the server (`GET`/`PUT /api/me/prefs`).
// The local cache (localStorage) stays the source for first paint; the server copy is
// what follows the user to another device.
//
// - Once someone is signed in, the server's copy wins if it has any prefs; if it has
//   none and the local choice isn't the default, the local choice is pushed up once.
// - A change is applied and cached at once (runtime.ts), then saved after a short pause.
// - A 404 means an older server: stay local-only, quietly. Other errors keep the local
//   choice and show up in `error`.
// - Signing out keeps the cache, so the sign-in page stays in the user's theme.
import { readonly, ref, watch } from "vue";

import { ApiError, api } from "../api";
import { useSession } from "../stores/session";
import { DEFAULT_PREFS, type UserPrefs } from "./index";
import { currentPrefs, onPrefsChange, parsePrefs, replacePrefs } from "./runtime";

export const SAVE_DELAY_MS = 400;

export type SyncStatus = "idle" | "saving" | "saved" | "error";

const status = ref<SyncStatus>("idle");
const error = ref("");

let timer: ReturnType<typeof setTimeout> | undefined;
/** The user changed something since the last fetch or save. */
let dirty = false;
/** An older server without the call: don't ask again this session. */
let localOnly = false;
let started = false;

const isDefault = (p: UserPrefs) => JSON.stringify(p) === JSON.stringify(DEFAULT_PREFS);

function fail(err: unknown): void {
  if (err instanceof ApiError && err.status === 404) {
    localOnly = true;
    status.value = "idle";
    return;
  }
  if (err instanceof ApiError && err.status === 401) {
    status.value = "idle";
    return;
  }
  status.value = "error";
  error.value =
    err instanceof ApiError && err.message
      ? `Couldn't save your preferences: ${err.message}`
      : "Couldn't save your preferences. They still apply on this device.";
}

async function push(): Promise<void> {
  if (localOnly) return;
  clearTimeout(timer);
  timer = undefined;
  dirty = false;
  status.value = "saving";
  error.value = "";
  try {
    await api.setPrefs(currentPrefs());
    if (status.value === "saving") status.value = "saved";
  } catch (err) {
    fail(err);
  }
}

function queue(): void {
  dirty = true;
  if (localOnly) return;
  clearTimeout(timer);
  status.value = "saving";
  error.value = "";
  timer = setTimeout(() => void push(), SAVE_DELAY_MS);
}

/** Fetches the server's copy for the signed-in user and reconciles it with the local one. */
export async function pullFromServer(): Promise<void> {
  if (localOnly) return;
  dirty = false;
  try {
    const { prefs } = await api.prefs();
    if (dirty) return; // the user changed something while this was in flight: theirs wins
    if (prefs && typeof prefs === "object" && Object.keys(prefs).length > 0) {
      replacePrefs(parsePrefs(prefs));
    } else if (!isDefault(currentPrefs())) {
      await push();
    }
  } catch (err) {
    fail(err);
  }
}

/** Call once, after Pinia is installed. */
export function initPrefsSync(): void {
  if (started) return;
  started = true;
  const session = useSession();
  onPrefsChange(() => {
    if (session.user) queue();
  });
  watch(
    () => session.user?.id,
    (id, was) => {
      // Another user's choices must not seed this one: with no saved prefs, pullFromServer would push them up.
      if (id && was && id !== was) replacePrefs(DEFAULT_PREFS);
      clearTimeout(timer);
      timer = undefined;
      status.value = "idle";
      error.value = "";
      if (id) void pullFromServer();
    },
    { immediate: true },
  );
}

export function usePrefsSync() {
  return { status: readonly(status), error: readonly(error) };
}
