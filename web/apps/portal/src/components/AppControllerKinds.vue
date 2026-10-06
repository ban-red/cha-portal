<script setup lang="ts">
import { KINDS, kindLabel as label, useControllerApps } from "../controllerKinds";
import FormError from "./FormError.vue";

// Which virtual controller each app sees (controllerKinds.ts); the app cards
// on the Environments page offer the same choice.

const { query, apps, missing, errors, choose } = useControllerApps();
</script>

<template>
  <section v-if="!missing && (query.isPending.value || query.isError.value || apps.length)" class="space-y-3" aria-labelledby="in-apps">
    <div class="space-y-1">
      <h2 id="in-apps" class="text-lg font-semibold tracking-tight">In your apps</h2>
      <p class="text-sm text-ink-2">
        Whatever controller you use, an app sees the one you pick here. A choice applies from the app's next launch.
      </p>
    </div>

    <p v-if="query.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="query.isError.value" :message="query.error.value?.message ?? 'Failed to load'" />
    <template v-else>
      <ul class="card divide-y divide-line">
        <li v-for="app in apps" :key="app.template" class="px-4 py-3 sm:px-5">
          <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
            <label :for="`${app.template}-kind`" class="text-sm font-medium">{{ app.name }}</label>
            <select
              :id="`${app.template}-kind`"
              :value="app.kind ?? ''"
              class="field w-auto"
              aria-describedby="kinds-about"
              @change="choose(app, $event)"
            >
              <option value="">Default ({{ label(app.default) }})</option>
              <option v-for="k in KINDS" :key="k.kind" :value="k.kind">{{ k.label }}</option>
            </select>
          </div>
          <div aria-live="polite">
            <FormError v-if="errors[app.template]" polite class="mt-2" :message="errors[app.template] ?? null" />
          </div>
        </li>
      </ul>
      <dl id="kinds-about" class="space-y-1 text-xs text-ink-2">
        <div v-for="k in KINDS" :key="k.kind" class="flex gap-2">
          <dt class="w-24 shrink-0 font-medium text-ink sm:w-28">{{ k.label }}</dt>
          <dd>{{ k.about }}</dd>
        </div>
      </dl>
    </template>
  </section>
</template>
