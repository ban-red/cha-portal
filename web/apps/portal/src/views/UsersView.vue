<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { ChevronDown, Eye, Plus, Trash2, X } from "lucide-vue-next";
import { ref } from "vue";
import { useRouter } from "vue-router";

import { ApiError, api, type Role } from "../api";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import UserAccessPanel from "../components/UserAccessPanel.vue";
import { ago, dateTime } from "../format";
import { useSession } from "../stores/session";

const queryClient = useQueryClient();
const session = useSession();
const router = useRouter();
const users = useQuery({ queryKey: ["users"], queryFn: api.users });

const showForm = ref(false);
const email = ref("");
const username = ref("");
const displayName = ref("");
const password = ref("");
const role = ref<Role>("user");
const formError = ref<string | null>(null);

const create = useMutation({
  mutationFn: () =>
    api.createUser({ email: email.value.trim() || undefined, username: username.value.trim() || undefined, displayName: displayName.value || undefined, password: password.value, role: role.value }),
  onSuccess: async () => {
    showForm.value = false;
    email.value = username.value = displayName.value = password.value = "";
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

// ---- view as, and manage access ----------------------------------------------------------

const managing = ref<string | null>(null);
const viewError = ref<string | null>(null);
const viewing = ref<string | null>(null);
async function viewAs(id: string) {
  viewing.value = id;
  viewError.value = null;
  try {
    await session.switchTo(id);
    await router.push({ name: "dashboard" });
  } catch (err) {
    viewError.value = err instanceof ApiError ? err.message : "Couldn't switch user.";
  } finally {
    viewing.value = null;
  }
}

const doomed = ref<{ id: string; displayName: string } | null>(null);
const deleteError = ref<string | null>(null);
const remove = useMutation({
  mutationFn: (id: string) => api.deleteUser(id),
  onSuccess: async () => {
    doomed.value = null;
    managing.value = null;
    await queryClient.invalidateQueries({ queryKey: ["users"] });
  },
  onError: (err) => {
    deleteError.value = err instanceof ApiError ? err.message : "Couldn't delete the user.";
  },
});

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
        <input id="u-name" v-model="username" name="username" class="field" autocomplete="off" autocapitalize="none" spellcheck="false" :required="!email.trim()" />
      </div>
      <div>
        <label class="label" for="u-email">Email <span class="font-normal text-ink-3">(optional)</span></label>
        <input id="u-email" v-model="email" name="email" type="email" class="field" autocomplete="off" autocapitalize="none" spellcheck="false" aria-describedby="u-email-hint" />
        <p id="u-email-hint" class="mt-1.5 text-xs text-ink-3">Without a username, the email is used. They can sign in with either.</p>
      </div>
      <div>
        <label class="label" for="u-display">Display name <span class="font-normal text-ink-3">(optional)</span></label>
        <input id="u-display" v-model="displayName" name="display-name" class="field" autocomplete="off" />
      </div>
      <div>
        <label class="label" for="u-pass">Password</label>
        <input id="u-pass" v-model="password" name="new-password" type="password" class="field" autocomplete="new-password" minlength="3" aria-describedby="u-pass-hint" required />
        <p id="u-pass-hint" class="mt-1.5 text-xs text-ink-3">At least 3 characters.</p>
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

    <FormError :message="viewError" />

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
        <tbody v-for="u in users.data.value" :key="u.id" class="border-b border-line/60 last:border-0">
          <tr>
            <td class="min-w-0 px-4 py-3 break-words sm:px-5">
              <p class="font-medium">{{ u.displayName }}</p>
              <p class="text-xs break-all text-ink-3">{{ u.username }}<span v-if="u.email && u.email !== u.username"> · {{ u.email }}</span><span v-if="u.disabled"> · disabled</span></p>
              <p class="mt-0.5 text-xs text-ink-3 sm:hidden" :title="dateTime(u.createdAt)">Created {{ ago(u.createdAt) }}</p>
              <div class="mt-2 flex flex-wrap gap-2">
                <button
                  v-if="u.id !== session.user?.id && !u.disabled"
                  type="button"
                  class="btn-ghost min-h-9 px-3 pointer-coarse:min-h-11"
                  :disabled="viewing !== null"
                  :aria-label="`View as ${u.displayName}`"
                  @click="viewAs(u.id)"
                >
                  <Eye class="size-4" aria-hidden="true" />
                  View as
                </button>
                <button
                  type="button"
                  class="btn-ghost min-h-9 px-3 pointer-coarse:min-h-11"
                  :aria-expanded="managing === u.id"
                  :aria-controls="`access-${u.id}`"
                  :aria-label="`Manage access for ${u.displayName}`"
                  @click="managing = managing === u.id ? null : u.id"
                >
                  <ChevronDown class="size-4 transition-transform" :class="managing === u.id ? '' : '-rotate-90'" aria-hidden="true" />
                  Manage access
                </button>
                <button
                  v-if="u.id !== session.user?.id"
                  type="button"
                  class="btn-ghost min-h-9 px-3 pointer-coarse:min-h-11"
                  :aria-label="`Delete ${u.displayName}`"
                  @click="((doomed = u), (deleteError = null))"
                >
                  <Trash2 class="size-4" aria-hidden="true" />
                  Delete
                </button>
              </div>
            </td>
            <td class="px-4 py-3 align-top sm:px-5 sm:align-middle">
              <span class="rounded-full border px-2.5 py-0.5 text-xs" :class="roleStyle[u.role]">{{ u.role }}</span>
            </td>
            <td class="hidden px-5 py-3 text-ink-2 sm:table-cell" :title="dateTime(u.createdAt)">{{ ago(u.createdAt) }}</td>
          </tr>
          <tr v-if="managing === u.id">
            <td :id="`access-${u.id}`" colspan="3" class="border-t border-line/60 bg-panel-2/40 px-4 py-4 sm:px-5">
              <UserAccessPanel :user="u" />
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <ConfirmDialog
      :open="!!doomed"
      :title="`Delete ${doomed?.displayName ?? ''}?`"
      :confirm-label="remove.isPending.value ? 'Deleting…' : 'Delete user'"
      :busy="remove.isPending.value"
      :error="deleteError"
      @cancel="doomed = null"
      @confirm="doomed && remove.mutate(doomed.id)"
    >
      Their account, sign-ins, devices and settings are removed. This can't be undone. Stop their running environments first.
    </ConfirmDialog>
  </div>
</template>
