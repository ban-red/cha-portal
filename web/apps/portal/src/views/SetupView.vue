<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";

import { ApiError } from "../api";
import AuthCard from "../components/AuthCard.vue";
import FormError from "../components/FormError.vue";
import { useSession } from "../stores/session";

const session = useSession();
const router = useRouter();
const token = ref("");
const username = ref("admin");
const displayName = ref("");
const password = ref("");
const confirm = ref("");
const error = ref<string | null>(null);
const busy = ref(false);

async function submit() {
  error.value = null;
  if (password.value !== confirm.value) {
    error.value = "The passwords don't match.";
    return;
  }
  busy.value = true;
  try {
    await session.setup(token.value, username.value, password.value, displayName.value || undefined);
    await router.replace({ name: "dashboard" });
  } catch (err) {
    error.value = err instanceof ApiError ? err.message : "Setup failed. Is cha-control running?";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <AuthCard title="Set up Cha Portal" subtitle="Create the first admin account.">
    <form class="space-y-4" @submit.prevent="submit">
      <div>
        <label class="label" for="token">Setup token</label>
        <input id="token" v-model="token" class="field font-mono" autocomplete="off" spellcheck="false" required />
        <p class="mt-1.5 text-xs text-ink-3">Printed in the cha-control log on first start.</p>
      </div>
      <div>
        <label class="label" for="username">Username</label>
        <input id="username" v-model="username" class="field" autocomplete="username" required />
      </div>
      <div>
        <label class="label" for="display">Display name <span class="normal-case text-ink-3">(optional)</span></label>
        <input id="display" v-model="displayName" class="field" autocomplete="name" />
      </div>
      <div>
        <label class="label" for="password">Password</label>
        <input id="password" v-model="password" type="password" class="field" autocomplete="new-password" minlength="10" required />
      </div>
      <div>
        <label class="label" for="confirm">Confirm password</label>
        <input id="confirm" v-model="confirm" type="password" class="field" autocomplete="new-password" required />
      </div>
      <FormError :message="error" />
      <button type="submit" class="btn-primary w-full" :disabled="busy">
        {{ busy ? "Creating…" : "Create admin" }}
      </button>
    </form>
  </AuthCard>
</template>
