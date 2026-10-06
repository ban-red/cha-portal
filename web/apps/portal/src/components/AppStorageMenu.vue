<script setup lang="ts">
// The storage button of an app card: a drop-down with the switch that keeps the app's
// data between launches, and a way to the full storage settings.
import { Database } from "lucide-vue-next";
import { onBeforeUnmount, ref, useId, watch } from "vue";

import type { StorageApp } from "../api";
import FormError from "./FormError.vue";
import ToggleSwitch from "./ToggleSwitch.vue";

const props = defineProps<{ app: StorageApp; error?: string | null; disabled?: boolean }>();
const emit = defineEmits<{ toggle: [persistent: boolean] }>();

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const button = ref<HTMLButtonElement | null>(null);
const id = useId();

function onPointer(e: PointerEvent) {
  if (root.value && !root.value.contains(e.target as Node)) open.value = false;
}
function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    open.value = false;
    button.value?.focus();
  }
}
watch(open, (o) => {
  if (o) {
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
  } else {
    document.removeEventListener("pointerdown", onPointer);
    document.removeEventListener("keydown", onKey);
  }
});
onBeforeUnmount(() => {
  document.removeEventListener("pointerdown", onPointer);
  document.removeEventListener("keydown", onKey);
});

const locked = () => props.disabled || props.app.live;
</script>

<template>
  <div ref="root" class="relative">
    <button
      ref="button"
      type="button"
      class="inline-flex size-9 items-center justify-center rounded-lg transition hover:bg-panel-2 pointer-coarse:size-11"
      :class="app.persistent ? 'text-accent' : 'text-ink-3 hover:text-ink'"
      aria-haspopup="true"
      :aria-expanded="open"
      :aria-controls="`${id}-menu`"
      :aria-label="`Storage for ${app.name}`"
      :title="app.persistent ? 'Data kept between launches' : 'Data not kept between launches'"
      @click="open = !open"
    >
      <Database class="size-5" aria-hidden="true" />
    </button>
    <div
      v-if="open"
      :id="`${id}-menu`"
      class="absolute right-0 z-20 mt-1 w-64 rounded-lg border border-line-strong bg-panel p-3 text-left shadow-lg"
    >
      <div class="flex items-center justify-between gap-3">
        <span :id="`${id}-label`" class="text-sm font-medium">Keep data</span>
        <ToggleSwitch
          :model-value="app.persistent"
          :disabled="locked()"
          :aria-labelledby="`${id}-label`"
          :aria-describedby="`${id}-hint`"
          @update:model-value="(v: boolean) => emit('toggle', v)"
        />
      </div>
      <p :id="`${id}-hint`" class="mt-1.5 text-xs text-ink-3">
        <template v-if="app.live">Stop {{ app.name }} to change this.</template>
        <template v-else-if="app.persistent">Settings, logins and files stay between launches.</template>
        <template v-else>{{ app.name }} starts fresh every time.</template>
      </p>
      <FormError v-if="error" polite class="mt-1.5" :message="error" />
      <RouterLink :to="{ name: 'storage' }" class="mt-2 inline-block text-xs text-accent hover:underline">
        Storage settings
      </RouterLink>
    </div>
  </div>
</template>
