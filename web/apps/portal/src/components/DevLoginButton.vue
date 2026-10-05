<script setup lang="ts">
// "Login as Local Dev": shown only when cha-control runs with `--dev-login`,
// plus a button per existing admin account, to sign in as the real thing.
import { onMounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { ApiError, api } from "../api";
import { useSession } from "../stores/session";
import FormError from "./FormError.vue";

const session = useSession();
const router = useRouter();
const route = useRoute();
const error = ref<string | null>(null);
const busy = ref(false);
const accounts = ref<{ username: string; displayName: string }[]>([]);

onMounted(async () => {
  if (!session.devLoginEnabled) return;
  try {
    accounts.value = (await api.devAccounts()).accounts;
  } catch {
    // Not offered (404 or 403): only the plain Local Dev button shows.
  }
});

async function signIn(username?: string) {
  error.value = null;
  busy.value = true;
  try {
    await session.devLogin(username);
    const next = typeof route.query.next === "string" && route.query.next.startsWith("/") ? route.query.next : "/";
    await router.replace(next);
  } catch (err) {
    error.value = err instanceof ApiError ? err.message : "Dev login failed. Is cha-control running?";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="session.devLoginEnabled" class="mt-5 space-y-3 border-t border-line pt-5">
    <FormError :message="error" />
    <button type="button" class="btn-ghost w-full" :disabled="busy" @click="signIn()">
      {{ busy ? "Signing in…" : "Login as Local Dev" }}
    </button>
    <button
      v-for="account in accounts"
      :key="account.username"
      type="button"
      class="btn-ghost w-full"
      :disabled="busy"
      @click="signIn(account.username)"
    >
      Login as {{ account.displayName }} ({{ account.username }})
    </button>
  </div>
</template>
