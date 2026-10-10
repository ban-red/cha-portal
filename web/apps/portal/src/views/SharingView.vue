<script setup lang="ts">
import { SESSION_ACCESS, accessNote } from "../shares";
import SegmentedControl from "../components/SegmentedControl.vue";
import { useTheme } from "../themes/runtime";
import { usePrefsSync } from "../themes/sync";

const { prefs, setPrefs } = useTheme();
const sync = usePrefsSync();
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <section class="card space-y-3 px-4 py-4 sm:px-5" aria-labelledby="access-title">
      <div>
        <h2 id="access-title" class="text-base font-semibold">Who can join your sessions</h2>
        <p class="mt-1 text-sm text-ink-2">
          What new environments you start allow. Anyone who joins starts out watching; you can change it per
          environment from the dashboard while it runs. Admins can always open any environment.
        </p>
      </div>
      <SegmentedControl
        label="Default access"
        :model-value="prefs.sessionAccess"
        :options="SESSION_ACCESS.map((a) => ({ value: a.value, label: a.label }))"
        @update:model-value="setPrefs({ sessionAccess: $event })"
      />
      <p class="text-sm text-ink-2" role="status">{{ accessNote(prefs.sessionAccess) }}</p>
      <p v-if="sync.error.value" class="text-sm text-danger" role="alert">{{ sync.error.value }}</p>
    </section>
  </div>
</template>
