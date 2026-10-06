<script setup lang="ts">
import { ref } from "vue";
import { useRouter } from "vue-router";

import { ApiError } from "../api";
import AuthCard from "../components/AuthCard.vue";
import DevLoginButton from "../components/DevLoginButton.vue";
import FormError from "../components/FormError.vue";
import { useSession } from "../stores/session";

const session = useSession();
const router = useRouter();
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
    await session.setup(username.value, password.value, displayName.value || undefined);
    await router.replace({ name: "dashboard" });
  } catch (err) {
    error.value = err instanceof ApiError ? err.message : "Setup failed. Is cha-control running?";
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <AuthCard title="Claim this portal" subtitle="Create your admin account to finish setting up Cha Portal.">
    <form class="space-y-4" @submit.prevent="submit">
      <div>
        <label class="label" for="username">Username</label>
        <input id="username" v-model="username" name="username" class="field" autocomplete="username" autocapitalize="none" spellcheck="false" minlength="3" autofocus required />
      </div>
      <div>
        <label class="label" for="display">Display name <span class="font-normal text-ink-3">(optional)</span></label>
        <input id="display" v-model="displayName" name="name" class="field" autocomplete="name" />
      </div>
      <div>
        <label class="label" for="password">Password</label>
        <input id="password" v-model="password" name="new-password" type="password" class="field" autocomplete="new-password" minlength="3" aria-describedby="password-hint" required />
        <p id="password-hint" class="mt-1.5 text-xs text-ink-3">At least 3 characters.</p>
      </div>
      <div>
        <label class="label" for="confirm">Confirm password</label>
        <input id="confirm" v-model="confirm" name="confirm-password" type="password" class="field" autocomplete="new-password" required />
      </div>
      <FormError :message="error" />
      <button type="submit" class="btn-primary w-full" :disabled="busy">
        {{ busy ? "Creating…" : "Create admin" }}
      </button>
    </form>
    <DevLoginButton />
  </AuthCard>
</template>
