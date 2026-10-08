<script setup lang="ts">
import type { ToolbarMenu } from "@cha/ui-spec";

import { MENU_BOX, SELECT } from "./toolbarClasses";

// The stream settings dropdown: the rows of the spec's "stream" menu (codec, frame rate, overlay,
// transport, the probe). What each says and when it shows is toolbar.json's.
defineProps<{ menu: ToolbarMenu }>();
const emit = defineEmits<{ row: [id: string, value?: string, el?: HTMLSelectElement] }>();

const picked = (id: string, e: Event) => {
  const el = e.target as HTMLSelectElement;
  emit("row", id, el.value, el);
};
</script>

<template>
  <div :id="menu.dom_id" :class="[MENU_BOX, 'left-0 space-y-2']">
    <template v-for="row in menu.rows" :key="row.id">
      <label v-if="row.kind === 'choice'" class="block">
        <span class="mb-0.5 block text-ink-2">{{ row.label }}</span>
        <select :class="SELECT" :disabled="row.disabled" :value="row.value" :title="row.tooltip ?? undefined" @change="picked(row.id, $event)">
          <option v-for="o in row.options" :key="o.value" :value="o.value" :disabled="o.disabled">{{ o.label }}</option>
        </select>
      </label>
      <button
        v-else-if="row.kind === 'button'"
        class="btn-ghost w-full px-3 py-1 text-xs"
        :disabled="row.disabled"
        :title="row.tooltip ?? undefined"
        @click="emit('row', row.id)"
      >
        {{ row.label }}
      </button>
    </template>
  </div>
</template>
