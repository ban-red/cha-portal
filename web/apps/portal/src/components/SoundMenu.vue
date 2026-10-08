<script setup lang="ts">
import type { ToolbarMenu } from "@cha/ui-spec";

import { MENU_BOX, TONE_TEXT } from "./toolbarClasses";

// The sound dropdown: on or off, the volume, restart. The rows are the spec's "sound" menu.
defineProps<{ menu: ToolbarMenu }>();
const emit = defineEmits<{ row: [id: string, event?: Event] }>();
</script>

<template>
  <div :id="menu.dom_id" :class="[MENU_BOX, 'left-0 space-y-2']">
    <template v-for="row in menu.rows" :key="row.id">
      <button
        v-if="row.id === 'sound-toggle'"
        class="btn-ghost w-full px-3 py-1 text-xs"
        :class="TONE_TEXT[row.tone]"
        :title="row.tooltip ?? undefined"
        @click="emit('row', row.id)"
      >
        {{ row.label }}
      </button>
      <label v-else-if="row.kind === 'slider' && row.slider" class="flex items-center gap-2">
        <span class="text-ink-2">{{ row.label }}</span>
        <input
          type="range"
          :min="row.slider.min"
          :max="row.slider.max"
          :step="row.slider.step"
          class="min-w-0 flex-1 accent-accent"
          :value="row.slider.value"
          :aria-valuetext="row.slider.aria_text"
          @input="emit('row', row.id, $event)"
        />
        <span class="w-8 text-right">{{ row.slider.display }}</span>
      </label>
      <button v-else-if="row.kind === 'button'" class="btn-ghost w-full px-3 py-1 text-xs" :title="row.tooltip ?? undefined" @click="emit('row', row.id)">
        {{ row.label }}
      </button>
    </template>
  </div>
</template>
