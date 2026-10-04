<script setup lang="ts">
// "Login as Local Dev": shown only when cha-control runs with `--dev-login`.
import { ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { ApiError } from "../api";
import { useSession } from "../stores/session";
import FormError from "./FormError.vue";

const session = useSession();
const router = useRouter();
const route = useRoute();
const error = ref<string | null>(null);
const busy = ref(false);

async function signIn() {
  error.value = null;
  busy.value = true;
  try {
    await session.devLogin();
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
    <button type="button" class="btn-ghost w-full" :disabled="busy" @click="signIn">
      {{ busy ? "Signing in…" : "Login as Local Dev" }}
    </button>
  </div>
</template>
