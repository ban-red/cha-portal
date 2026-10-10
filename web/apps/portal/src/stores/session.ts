import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { ApiError, api, type Impersonator, type Me, type User } from "../api";
import { queryClient } from "../queryClient";

/** Who is signed in, and whether the portal still needs its first admin. */
export const useSession = defineStore("session", () => {
  const user = ref<User | null>(null);
  /** The admin behind a "view as" session; null when signed in as yourself. */
  const impersonator = ref<Impersonator | null>(null);
  const setupNeeded = ref(false);
  /** The server runs with `--dev-login`. */
  const devLoginEnabled = ref(false);
  const loaded = ref(false);
  const isAdmin = computed(() => user.value?.role === "admin");
  /** May open the user switcher: an admin, or an admin viewing as someone else. */
  const canSwitch = computed(() => isAdmin.value || impersonator.value !== null);

  async function load(force = false): Promise<void> {
    if (loaded.value && !force) return;
    const status = await api.setupStatus();
    setupNeeded.value = status.needed;
    devLoginEnabled.value = status.devLogin;
    applyMe(status.needed ? null : await currentUser());
    loaded.value = true;
  }

  function applyMe(me: Me | null) {
    if (!me) {
      user.value = null;
      impersonator.value = null;
      return;
    }
    const { impersonator: by, ...rest } = me;
    user.value = rest;
    impersonator.value = by ?? null;
  }

  async function currentUser(): Promise<Me | null> {
    try {
      return await api.me();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return null;
      throw err;
    }
  }

  async function login(username: string, password: string): Promise<void> {
    user.value = await api.login({ username, password });
    impersonator.value = null;
  }

  async function devLogin(username?: string): Promise<void> {
    user.value = await api.devLogin(username);
    impersonator.value = null;
    setupNeeded.value = false;
  }

  async function setup(username: string, password: string, displayName?: string): Promise<void> {
    user.value = await api.setup({ username, password, displayName });
    setupNeeded.value = false;
  }

  async function logout(): Promise<void> {
    await api.logout();
    user.value = null;
    impersonator.value = null;
    queryClient.clear();
  }

  /** Everything cached belongs to the previous user. */
  async function reload(): Promise<void> {
    const me = await currentUser();
    // Empty the cache first: the pages are rebuilt for the new user (App.vue) and must not see the old one's data.
    queryClient.clear();
    applyMe(me);
  }

  /** View the portal as another user (admins; 12 hours). */
  async function switchTo(userId: string): Promise<void> {
    await api.switchUser(userId);
    await reload();
  }

  /** Back to the admin who switched. */
  async function switchBack(): Promise<void> {
    await api.switchBack();
    await reload();
  }

  return { user, setupNeeded, devLoginEnabled, loaded, isAdmin, impersonator, canSwitch, load, switchTo, switchBack, login, devLogin, setup, logout };
});
