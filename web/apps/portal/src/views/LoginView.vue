<script setup lang="ts">
import { ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { ApiError } from "../api";
import AuthCard from "../components/AuthCard.vue";
import DevLoginButton from "../components/DevLoginButton.vue";
import FormError from "../components/FormError.vue";
import { useSession } from "../stores/session";

const session = useSession();
const router = useRouter();
const route = useRoute();
const username = ref("");
const password = ref("");
const error = ref<string | null>(null);
const busy = ref(false);

async function submit() {
  error.value = null;
  busy.value = true;
  try {
    await session.login(username.value, password.value);
    const next = typeof route.query.next === "string" && route.query.next.startsWith("/") ? route.query.next : "/";
    await router.replace(next);
  } catch (err) {
    error.value = err instanceof ApiError ? err.message : "Sign-in failed. Is cha-control running?";
    password.value = "";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <AuthCard title="Sign in to Cha Portal">
    <form class="space-y-4" @submit.prevent="submit">
      <div>
        <label class="label" for="username">Username</label>
        <input id="username" v-model="username" class="field" autocomplete="username" autofocus required />
      </div>
      <div>
        <label class="label" for="password">Password</label>
        <input id="password" v-model="password" type="password" class="field" autocomplete="current-password" required />
      </div>
      <FormError :message="error" />
      <button type="submit" class="btn-primary w-full" :disabled="busy">{{ busy ? "Signing in…" : "Sign in" }}</button>
    </form>
    <DevLoginButton />
  </AuthCard>
</template>
