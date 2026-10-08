<script setup lang="ts">
import { RotateCcw } from "lucide-vue-next";
import { useId } from "vue";

// One field of a custom environment: the base's value when it follows the base, an editor
// (the default slot, given the input's `id`) with a reset when it is overridden.
defineProps<{
  label: string;
  /** The base's value, in words. */
  inherited: string;
  overridden: boolean;
  hint?: string;
}>();
const emit = defineEmits<{ override: []; reset: [] }>();
const id = useId();
</script>

<template>
  <div class="grid gap-x-4 gap-y-1.5 py-3 sm:grid-cols-[11rem_minmax(0,1fr)]">
    <div class="flex items-center gap-2 sm:block">
      <label :for="id" class="text-sm font-medium text-ink-2">{{ label }}</label>
      <p class="text-2xs sm:mt-0.5" :class="overridden ? 'text-accent' : 'text-ink-3'">{{ overridden ? "Changed" : "From the base" }}</p>
    </div>
    <div class="min-w-0 space-y-1.5">
      <template v-if="overridden">
        <div class="flex flex-wrap items-center gap-2">
          <div class="min-w-0 flex-1 basis-56"><slot :id="id" /></div>
          <button type="button" class="btn-ghost shrink-0 px-3 py-1 text-xs" :aria-label="`Reset ${label} to the base's`" @click="emit('reset')">
            <RotateCcw class="size-3.5" aria-hidden="true" />
            Reset
          </button>
        </div>
        <p class="text-xs text-ink-3">Base: <span class="break-all">{{ inherited }}</span></p>
      </template>
      <div v-else class="flex flex-wrap items-center gap-2">
        <p :id="id" class="min-w-0 flex-1 basis-56 text-sm break-all text-ink-2">{{ inherited }}</p>
        <button type="button" class="btn-ghost shrink-0 px-3 py-1 text-xs" :aria-label="`Change ${label}`" @click="emit('override')">Change</button>
      </div>
      <p v-if="hint" class="text-xs text-ink-3">{{ hint }}</p>
    </div>
  </div>
</template>
