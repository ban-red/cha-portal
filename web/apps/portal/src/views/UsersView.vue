<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Plus, X } from "lucide-vue-next";
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
  admin: "border-accent/40 bg-accent-soft text-accent",
  user: "border-line-strong text-ink-2",
  guest: "border-line-strong text-ink-2",
};
</script>

<template>
  <div class="max-w-5xl space-y-6">
    <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
      <p class="min-w-0 text-sm text-ink-2">People who can sign in to this portal.</p>
      <button type="button" :class="showForm ? 'btn-ghost' : 'btn-primary'" :aria-expanded="showForm" aria-controls="add-user" @click="showForm = !showForm">
        <X v-if="showForm" class="size-4" aria-hidden="true" />
        <Plus v-else class="size-4" aria-hidden="true" />
        {{ showForm ? "Cancel" : "Add user" }}
      </button>
    </div>

    <form v-if="showForm" id="add-user" class="card grid max-w-3xl gap-4 p-5 sm:grid-cols-2" @submit.prevent="submit">
      <div>
        <label class="label" for="u-name">Username</label>
        <input id="u-name" v-model="username" name="username" class="field" autocomplete="off" autocapitalize="none" spellcheck="false" required />
      </div>
      <div>
        <label class="label" for="u-display">Display name <span class="font-normal text-ink-3">(optional)</span></label>
        <input id="u-display" v-model="displayName" name="display-name" class="field" autocomplete="off" />
      </div>
      <div>
        <label class="label" for="u-pass">Password</label>
        <input id="u-pass" v-model="password" name="new-password" type="password" class="field" autocomplete="new-password" minlength="10" aria-describedby="u-pass-hint" required />
        <p id="u-pass-hint" class="mt-1.5 text-xs text-ink-3">At least 10 characters.</p>
      </div>
      <div>
        <label class="label" for="u-role">Role</label>
        <select id="u-role" v-model="role" class="field">
          <option value="user">User</option>
          <option value="guest">Guest</option>
          <option value="admin">Admin</option>
        </select>
      </div>
      <div class="space-y-3 sm:col-span-2">
        <FormError :message="formError" />
        <button type="submit" class="btn-primary max-sm:w-full" :disabled="create.isPending.value">
          {{ create.isPending.value ? "Creating…" : "Create user" }}
        </button>
      </div>
    </form>

    <!-- Two columns on a phone (the date moves under the name), three from sm up, so there
         is no sideways scroll; long names wrap instead. -->
    <div class="card overflow-hidden">
      <p v-if="users.isPending.value" class="p-6 text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="users.isError.value" class="m-4" :message="users.error.value?.message ?? 'Failed to load'" />
      <table v-else class="w-full text-sm">
        <caption class="sr-only">
          Users
        </caption>
        <thead class="border-b border-line text-left text-xs text-ink-2">
          <tr>
            <th scope="col" class="px-4 py-3 font-medium sm:px-5">User</th>
            <th scope="col" class="px-4 py-3 font-medium sm:px-5">Role</th>
            <th scope="col" class="hidden px-5 py-3 font-medium sm:table-cell">Created</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="u in users.data.value" :key="u.id" class="border-b border-line/60 last:border-0">
            <td class="min-w-0 px-4 py-3 break-words sm:px-5">
              <p class="font-medium">{{ u.displayName }}</p>
              <p class="text-xs break-all text-ink-3">{{ u.username }}<span v-if="u.disabled"> · disabled</span></p>
              <p class="mt-0.5 text-xs text-ink-3 sm:hidden" :title="dateTime(u.createdAt)">Created {{ ago(u.createdAt) }}</p>
            </td>
            <td class="px-4 py-3 align-top sm:px-5 sm:align-middle">
              <span class="rounded-full border px-2.5 py-0.5 text-xs" :class="roleStyle[u.role]">{{ u.role }}</span>
            </td>
            <td class="hidden px-5 py-3 text-ink-2 sm:table-cell" :title="dateTime(u.createdAt)">{{ ago(u.createdAt) }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
