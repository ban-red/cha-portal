import { defineStore } from "pinia";
import { computed, ref } from "vue";

import { ApiError, api, type User } from "../api";

/** Who is signed in, and whether the portal still needs its first admin. */
export const useSession = defineStore("session", () => {
  const user = ref<User | null>(null);
  const setupNeeded = ref(false);
  /** The server runs with `--dev-login`. */
  const devLoginEnabled = ref(false);
  const loaded = ref(false);
  const isAdmin = computed(() => user.value?.role === "admin");

  async function load(force = false): Promise<void> {
    if (loaded.value && !force) return;
    const status = await api.setupStatus();
    setupNeeded.value = status.needed;
    devLoginEnabled.value = status.devLogin;
    user.value = status.needed ? null : await currentUser();
    loaded.value = true;
  }

  async function currentUser(): Promise<User | null> {
    try {
      return await api.me();
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) return null;
      throw err;
    }
  }

  async function login(username: string, password: string): Promise<void> {
    user.value = await api.login({ username, password });
  }

  async function devLogin(username?: string): Promise<void> {
    user.value = await api.devLogin(username);
    setupNeeded.value = false;
  }

  async function setup(token: string, username: string, password: string, displayName?: string): Promise<void> {
    user.value = await api.setup({ token, username, password, displayName });
    setupNeeded.value = false;
  }

  async function logout(): Promise<void> {
    await api.logout();
    user.value = null;
  }

  return { user, setupNeeded, devLoginEnabled, loaded, isAdmin, load, login, devLogin, setup, logout };
});
