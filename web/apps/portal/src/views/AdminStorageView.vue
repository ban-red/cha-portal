<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, reactive, ref } from "vue";

import { api, type AdminStorageApp, type SharedAccess } from "../api";
import FormError from "../components/FormError.vue";
import NotAvailable from "../components/NotAvailable.vue";
import ToggleSwitch from "../components/ToggleSwitch.vue";
import {
  ADMIN_STORAGE_KEY,
  SHARED_OPTIONS,
  STORAGE_KEY,
  notAvailable,
  patchApp,
  serverMessage,
  sharedPath,
} from "../storage";

const queryClient = useQueryClient();
const storage = useQuery({ queryKey: ADMIN_STORAGE_KEY, queryFn: api.adminStorage });
const apps = computed(() => storage.data.value?.apps ?? []);
const root = computed(() => (storage.data.value?.root ?? "").replace(/\/+$/, ""));

// A failed save, per app, in a live region next to its controls.
const errors = reactive<Record<string, string>>({});
const inflight = ref(0);

type Change = { defaultPersistent?: boolean; sharedAccess?: SharedAccess };

// Each change saves on its own: the UI follows it at once and goes back if the server refuses.
const save = useMutation({
  mutationFn: (v: { app: AdminStorageApp; change: Change }) => api.setAdminStorage(v.app.template, v.change),
  onMutate: async ({ app, change }) => {
    delete errors[app.template];
    inflight.value++;
    await queryClient.cancelQueries({ queryKey: ADMIN_STORAGE_KEY });
    const was: Change = {};
    if (change.defaultPersistent !== undefined) was.defaultPersistent = app.defaultPersistent;
    if (change.sharedAccess !== undefined) was.sharedAccess = app.sharedAccess;
    patchApp<AdminStorageApp>(queryClient, ADMIN_STORAGE_KEY, app.template, change);
    return { was };
  },
  onSuccess: (updated, { app }) => patchApp<AdminStorageApp>(queryClient, ADMIN_STORAGE_KEY, app.template, updated),
  onError: (err, { app }, ctx) => {
    patchApp<AdminStorageApp>(queryClient, ADMIN_STORAGE_KEY, app.template, ctx?.was ?? {});
    errors[app.template] = notAvailable(err)
      ? "Not available on this server yet."
      : `Couldn't save the change: ${serverMessage(err, "try again")}`;
  },
  onSettled: () => {
    inflight.value--;
    if (inflight.value > 0) return;
    void queryClient.invalidateQueries({ queryKey: ADMIN_STORAGE_KEY });
    // Users who haven't chosen for themselves follow the default.
    void queryClient.invalidateQueries({ queryKey: STORAGE_KEY });
  },
});

function setSharedAccess(app: AdminStorageApp, event: Event) {
  const sharedAccess = (event.target as HTMLSelectElement).value as SharedAccess;
  if (sharedAccess !== app.sharedAccess) save.mutate({ app, change: { sharedAccess } });
}
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-6">
    <div class="space-y-1 text-sm text-ink-2">
      <p>
        What each app does by default. Users can still choose for themselves under
        <RouterLink to="/settings/storage" class="text-accent hover:underline">Storage</RouterLink>.
      </p>
      <p>Shared data is one folder every user can use, instead of each keeping their own copy.</p>
    </div>

    <p v-if="storage.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <NotAvailable v-else-if="notAvailable(storage.error.value)" />
    <FormError v-else-if="storage.isError.value" :message="storage.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!apps.length" class="card px-6 py-10 text-center">
      <p class="font-medium">No apps</p>
      <p class="mt-1 text-sm text-ink-2">Nothing here keeps data yet.</p>
    </div>

    <ul v-else class="card divide-y divide-line">
      <li v-for="app in apps" :key="app.template" class="px-4 py-4 sm:px-5">
        <h2 :id="`${app.template}-name`" class="font-semibold">{{ app.name }}</h2>
        <p v-if="app.sharedAccess !== 'none'" class="mt-0.5 font-mono text-xs break-all text-ink-3">
          {{ sharedPath(root, app) }}
          <span v-if="app.sharedPath" class="font-sans">· set on the node (CHA_SHARED_DIRS)</span>
        </p>

        <div class="mt-3 flex flex-wrap items-center gap-x-8 gap-y-3">
          <div class="flex items-center gap-3">
            <ToggleSwitch
              :model-value="app.defaultPersistent"
              :aria-labelledby="`${app.template}-name ${app.template}-default`"
              @update:model-value="save.mutate({ app, change: { defaultPersistent: $event } })"
            />
            <span :id="`${app.template}-default`" class="text-sm">Default: keep data</span>
          </div>
          <div class="flex items-center gap-3">
            <label :for="`${app.template}-shared`" class="text-sm">
              <span class="sr-only">{{ app.name }}: </span>Shared data
            </label>
            <select
              :id="`${app.template}-shared`"
              :value="app.sharedAccess"
              class="field w-auto"
              :aria-describedby="app.template === 'steam' ? `${app.template}-shared-hint` : undefined"
              @change="setSharedAccess(app, $event)"
            >
              <option v-for="o in SHARED_OPTIONS" :key="o.value" :value="o.value">{{ o.label }}</option>
            </select>
          </div>
        </div>
        <p v-if="app.template === 'steam'" :id="`${app.template}-shared-hint`" class="mt-2 text-xs text-ink-3">
          Games are installed once for everyone. Steam needs read and write, and users who keep their data.
          Experimental.
        </p>

        <div aria-live="polite">
          <FormError v-if="errors[app.template]" polite class="mt-3" :message="errors[app.template] ?? null" />
        </div>
      </li>
    </ul>
  </div>
</template>
