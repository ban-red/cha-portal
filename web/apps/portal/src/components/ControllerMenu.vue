<script setup lang="ts">
import type { ToolbarMenu } from "@cha/ui-spec";
import { computed } from "vue";

import { MENU_BOX } from "./toolbarClasses";

// The controllers dropdown: who is sending input, and how to connect one. The rows are the spec's
// "controllers" menu.
const props = defineProps<{ menu: ToolbarMenu }>();
const emit = defineEmits<{ row: [id: string] }>();

const rowOf = (id: string) => props.menu.rows.find((r) => r.id === id);
const reason = computed(() => rowOf("hid-reason"));
</script>

<template>
  <div :id="menu.dom_id" :class="[MENU_BOX, 'right-0 p-3']">
    <ul v-if="rowOf('list')" class="mb-3 space-y-1">
      <li v-for="(c, i) in rowOf('list')!.items" :key="i" class="flex justify-between gap-2">
        <span class="truncate">{{ c.text }}</span>
        <span class="shrink-0 text-ink-3">{{ c.detail }}</span>
      </li>
    </ul>
    <p v-else-if="rowOf('empty-note')" class="mb-3 text-ink-2">{{ rowOf("empty-note")!.text }}</p>
    <div class="flex flex-wrap items-center gap-2">
      <button
        v-if="rowOf('connect')"
        class="btn-ghost px-3 py-1 text-xs"
        :disabled="rowOf('connect')!.disabled"
        :aria-describedby="reason?.dom_id ?? undefined"
        @click="emit('row', 'connect')"
      >
        {{ rowOf("connect")!.label }}
      </button>
      <RouterLink v-if="rowOf('controllers-page')" :to="rowOf('controllers-page')!.to!" class="text-accent hover:underline">{{ rowOf("controllers-page")!.label }}</RouterLink>
    </div>
    <p v-if="reason" :id="reason.dom_id ?? undefined" class="mt-2 text-ink-3">{{ reason.text }}</p>
    <p v-if="rowOf('connect-note')" role="status" class="mt-2 text-warn">{{ rowOf("connect-note")!.text }}</p>
  </div>
</template>
