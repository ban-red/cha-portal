<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { ref } from "vue";

import { ApiError, api, type Role } from "../api";
import FormError from "../components/FormError.vue";
import { ago, dateTime } from "../format";

const queryClient = useQueryClient();
const users = useQuery({ queryKey: ["users"], queryFn: api.users });

const showForm = ref(false);
const username = ref("");
const displayName = ref("");
const password = ref("");
const role = ref<Role>("user");
const formError = ref<string | null>(null);

const create = useMutation({
  mutationFn: () =>
    api.createUser({ username: username.value, displayName: displayName.value || undefined, password: password.value, role: role.value }),
  onSuccess: async () => {
    showForm.value = false;
    username.value = displayName.value = password.value = "";
    role.value = "user";
    await queryClient.invalidateQueries({ queryKey: ["users"] });
  },
  onError: (err) => {
    formError.value = err instanceof ApiError ? err.message : "Couldn't create the user.";
  },
});

function submit() {
  formError.value = null;
  create.mutate();
}

const roleStyle: Record<Role, string> = {
  admin: "border-accent/40 text-accent",
  user: "border-line text-ink-2",
  guest: "border-line text-ink-3",
};
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-6">
    <div class="flex items-center justify-between gap-4">
      <p class="text-sm text-ink-2">People who can sign in to this portal.</p>
      <button class="btn-primary" @click="showForm = !showForm">{{ showForm ? "Cancel" : "Add user" }}</button>
    </div>

    <form v-if="showForm" class="card grid gap-4 p-5 sm:grid-cols-2" @submit.prevent="submit">
      <div>
        <label class="label" for="u-name">Username</label>
        <input id="u-name" v-model="username" class="field" autocomplete="off" required />
      </div>
      <div>
        <label class="label" for="u-display">Display name <span class="normal-case text-ink-3">(optional)</span></label>
        <input id="u-display" v-model="displayName" class="field" autocomplete="off" />
      </div>
      <div>
        <label class="label" for="u-pass">Password</label>
        <input id="u-pass" v-model="password" type="password" class="field" autocomplete="new-password" minlength="10" required />
      </div>
      <div>
        <label class="label" for="u-role">Role</label>
        <select id="u-role" v-model="role" class="field">
          <option value="user">User</option>
          <option value="guest">Guest</option>
          <option value="admin">Admin</option>
        </select>
      </div>
      <div class="sm:col-span-2 space-y-3">
        <FormError :message="formError" />
        <button type="submit" class="btn-primary" :disabled="create.isPending.value">
          {{ create.isPending.value ? "Creating…" : "Create user" }}
        </button>
      </div>
    </form>

    <div class="card overflow-hidden">
      <p v-if="users.isPending.value" class="p-6 text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="users.isError.value" class="m-4" :message="users.error.value?.message ?? 'Failed to load'" />
      <table v-else class="w-full text-sm">
        <thead class="border-b border-line text-left text-xs tracking-wide text-ink-3 uppercase">
          <tr>
            <th class="px-5 py-3 font-medium">User</th>
            <th class="px-5 py-3 font-medium">Role</th>
            <th class="hidden px-5 py-3 font-medium sm:table-cell">Created</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="u in users.data.value" :key="u.id" class="border-b border-line/60 last:border-0">
            <td class="px-5 py-3">
              <p class="font-medium">{{ u.displayName }}</p>
              <p class="text-xs text-ink-3">{{ u.username }}<span v-if="u.disabled"> · disabled</span></p>
            </td>
            <td class="px-5 py-3">
              <span class="rounded-full border px-2 py-0.5 text-xs" :class="roleStyle[u.role]">{{ u.role }}</span>
            </td>
            <td class="hidden px-5 py-3 text-ink-2 sm:table-cell" :title="dateTime(u.createdAt)">{{ ago(u.createdAt) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
