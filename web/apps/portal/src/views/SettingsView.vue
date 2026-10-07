<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, ref } from "vue";

import { api, ApiError } from "../api";
import FormError from "../components/FormError.vue";
import NotAvailable from "../components/NotAvailable.vue";
import { notAvailable } from "../storage";

const SETTINGS_KEY = ["admin-settings"] as const;
const queryClient = useQueryClient();
const settings = useQuery({ queryKey: SETTINGS_KEY, queryFn: api.adminSettings });

// Minutes; 0 is off. A value set some other way still shows (and stays) as itself.
const PRESETS = [0, 5, 10, 15, 30, 60, 120, 240, 480, 1440];
const minutes = computed(() => settings.data.value?.idleShutdownMinutes ?? 30);
const options = computed(() => [...new Set([...PRESETS, minutes.value])].sort((a, b) => a - b));

function label(m: number): string {
  if (m === 0) return "Never";
  if (m < 60) return `${m} minutes`;
  if (m % 60 === 0) return m === 60 ? "1 hour" : m === 1440 ? "1 day" : `${m / 60} hours`;
  return `${m} minutes`;
}

const error = ref<string | null>(null);
const saved = ref(false);
let savedTimer: ReturnType<typeof setTimeout> | undefined;

const save = useMutation({
  mutationFn: (idleShutdownMinutes: number) => api.setAdminSettings({ idleShutdownMinutes }),
  onMutate: () => {
    error.value = null;
    saved.value = false;
  },
  onSuccess: (data) => {
    queryClient.setQueryData(SETTINGS_KEY, data);
    saved.value = true;
    clearTimeout(savedTimer);
    savedTimer = setTimeout(() => (saved.value = false), 2500);
  },
  onError: (err) => {
    error.value = err instanceof ApiError ? err.message : "Couldn't save the setting.";
  },
});

function onChange(event: Event) {
  const value = Number((event.target as HTMLSelectElement).value);
  if (value !== minutes.value) save.mutate(value);
}
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <p v-if="settings.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <NotAvailable v-else-if="notAvailable(settings.error.value)" what="portal settings" />
    <FormError v-else-if="settings.isError.value" :message="settings.error.value?.message ?? 'Failed to load'" />

    <section v-else class="card px-4 py-4 sm:px-5" aria-labelledby="idle-heading">
      <h2 id="idle-heading" class="text-base font-semibold">Idle shutoff</h2>
      <p class="mt-1 text-sm text-ink-2">
        Stop a running app when nobody has used it for this long. It frees the GPU for others; the app's data is kept
        if its owner keeps data for it. The wait starts when the last person stops using it, or when the app starts if
        nobody connects.
      </p>
      <div class="mt-4 flex flex-wrap items-center gap-x-3 gap-y-2">
        <label for="idle-shutdown" class="text-sm">Stop apps nobody is using after</label>
        <select id="idle-shutdown" class="field w-auto" :value="minutes" :disabled="save.isPending.value" @change="onChange">
          <option v-for="m in options" :key="m" :value="m">{{ label(m) }}</option>
        </select>
        <span v-if="saved" class="text-sm text-ok" role="status">Saved</span>
      </div>
      <p class="mt-2 text-xs text-ink-3">
        The default is 30 minutes. A browser tab that is hidden counts as unused unless it is playing sound; native apps
        and Moonlight count while connected. Apps on an older streamer can't tell hidden tabs apart, so any open tab
        counts, and the wait starts over if the portal restarts.
      </p>
      <div aria-live="polite"><FormError v-if="error" polite class="mt-3" :message="error" /></div>
    </section>
  </div>
</template>
